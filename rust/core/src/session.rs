//! Stream sessions: launches, live sessions and their statistics, and the
//! user-facing warnings a session raises.
use crate::{rtsp::Negotiated, state::Client};
use anyhow::{Result, bail};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Stream,
    RemoteMonitor,
    InputOnly,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Warning {
    pub code: String,
    pub message: String,
}

/// How long a dropped packet, frame or input stays on the stream card after
/// it last happened.
pub const EVENT_PERIOD: Duration = Duration::from_secs(30);

/// Current warnings by condition code, each with an optional expiry.
type WarningEntries = BTreeMap<String, (String, Option<Instant>)>;

#[derive(Default)]
pub struct Warnings {
    entries: std::sync::Mutex<WarningEntries>,
    /// A bit per present code's hash: clearing an absent code, as per-frame
    /// recovery paths do on a healthy stream, takes no lock.
    present: AtomicU64,
}
fn warning_bit(code: &str) -> u64 {
    let hash = code.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    1 << (hash % 64)
}
fn warning_bits(entries: &WarningEntries) -> u64 {
    entries
        .keys()
        .fold(0, |bits, code| bits | warning_bit(code))
}
impl Warnings {
    /// Keep one current warning per condition, without flooding a retry loop.
    pub fn set(&self, code: &str, message: impl AsRef<str>) {
        self.insert(code, message.as_ref(), None);
    }
    /// A recurring event rather than a lasting condition: shown until
    /// `period` passes without it, and logged again only after that.
    pub fn event(&self, code: &str, message: impl AsRef<str>, period: Duration) {
        self.insert(code, message.as_ref(), Some(Instant::now() + period));
    }
    fn insert(&self, code: &str, message: &str, expires: Option<Instant>) {
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap();
        match entries.get_mut(code) {
            Some((current, until)) if current == message && until.is_none_or(|at| at > now) => {
                *until = expires;
            }
            _ => {
                tracing::warn!(code, "{message}");
                entries.insert(code.into(), (message.into(), expires));
                self.present.fetch_or(warning_bit(code), Ordering::Release);
            }
        }
    }
    pub fn clear(&self, code: &str) {
        if self.present.load(Ordering::Acquire) & warning_bit(code) == 0 {
            return;
        }
        let mut entries = self.entries.lock().unwrap();
        if entries.remove(code).is_some() {
            self.present
                .store(warning_bits(&entries), Ordering::Release);
        }
    }
    pub fn snapshot(&self) -> Vec<Warning> {
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap();
        let before = entries.len();
        entries.retain(|_, (_, until)| until.is_none_or(|at| at > now));
        if entries.len() != before {
            self.present
                .store(warning_bits(&entries), Ordering::Release);
        }
        entries
            .iter()
            .map(|(code, (message, _))| Warning {
                code: code.clone(),
                message: message.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn events_expire_and_clearing_absent_codes_skips_the_lock() {
        let warnings = Warnings::default();
        warnings.clear("encoder_recovery");
        assert_eq!(warnings.present.load(Ordering::Relaxed), 0);
        warnings.set("display_virtual", "Physical display in use");
        warnings.event("network_send", "Packets dropped", Duration::ZERO);
        // An expired event disappears; a lasting condition stays.
        assert_eq!(
            warnings
                .snapshot()
                .iter()
                .map(|w| w.code.as_str())
                .collect::<Vec<_>>(),
            ["display_virtual"]
        );
        assert_eq!(
            warnings.present.load(Ordering::Relaxed),
            warning_bit("display_virtual")
        );
        warnings.event("network_send", "Packets dropped", Duration::from_secs(60));
        assert_eq!(warnings.snapshot().len(), 2);
        warnings.clear("display_virtual");
        warnings.clear("network_send");
        assert!(warnings.snapshot().is_empty());
        assert_eq!(warnings.present.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn warnings_survive_preparation_and_clear_after_recovery() {
        let launch = launch("warnings", Role::Stream);
        launch.warnings.set("display", "Physical display in use");
        let session = Session::new(launch.clone(), Negotiated::default());
        *session.encoder.write().unwrap() = "amf".into();
        let capture = Arc::new(Warnings::default());
        *session.capture_warnings.write().unwrap() = capture.clone();
        capture.set("capture", "Desktop Duplication in use");
        launch.warnings.set("display", "Physical display in use");
        assert_eq!(session.info()["encoder"], "amf");
        assert_eq!(
            session.info()["warnings"],
            serde_json::json!([
                {"code":"display", "message":"Physical display in use"},
                {"code":"capture", "message":"Desktop Duplication in use"}
            ])
        );
        capture.set("capture", "Capture recovering");
        assert_eq!(
            session.info()["warnings"][1]["message"],
            "Capture recovering"
        );
        capture.clear("capture");
        launch.warnings.clear("display");
        assert_eq!(session.info()["warnings"], serde_json::json!([]));
        let other = Session::new(
            super::tests::launch("other", Role::Stream),
            Negotiated::default(),
        );
        launch.warnings.set("audio", "Audio interrupted");
        assert_eq!(other.info()["warnings"], serde_json::json!([]));
    }
    #[test]
    fn client_fec_is_exposed_as_telemetry_without_changing_stream_decisions() {
        let session = Session::new(launch("fec", Role::Stream), Negotiated::default());
        session.idr.store(false, Ordering::Relaxed);
        session.stats.video_frame.store(8, Ordering::Release);
        let bitrate = session.bitrate.load(Ordering::Relaxed);
        let report = [
            0, 0, 0, 7, 0, 11, 0, 0, 0, 1, 0, 10, 0, 2, 0, 9, 0, 1, 20, 0, 1,
        ];
        session.record_fec_status(&report);
        session.record_fec_status(&report[..20]);
        let info = session.info();
        assert_eq!(info["performance"]["fec_reports"], 2);
        assert_eq!(info["performance"]["fec_recovered_frames"], 1);
        assert_eq!(info["performance"]["fec_invalid_reports"], 1);
        assert_eq!(session.bitrate.load(Ordering::Relaxed), bitrate);
        assert!(!session.idr.load(Ordering::Relaxed));
        assert!(session.invalidation.lock().unwrap().is_none());
        assert_eq!(info["idr_requests"], 0);
        assert_eq!(info["reference_invalidations"], 0);
        assert!(!session.stopping());
    }
    #[test]
    fn a_launch_still_being_prepared_does_not_expire() {
        let mut sessions = Sessions::default();
        let mut slow = launch("slow", Role::Stream);
        slow.created = Instant::now() - Duration::from_secs(90);
        slow.preparing.store(true, Ordering::Release);
        sessions.pending.insert(slow.id.clone(), slow.clone());
        drop(sessions.expire());
        assert!(sessions.pending.contains_key("slow"));
        // Prepared: the client's 30 s count from then.
        slow.preparing.store(false, Ordering::Release);
        drop(sessions.expire());
        assert!(!sessions.pending.contains_key("slow"));
    }
    #[test]
    fn expired_launches_restore_resources_after_unlocking_sessions() {
        use std::sync::{Mutex, Weak};
        struct Restore {
            sessions: Weak<Mutex<Sessions>>,
            restored: Arc<AtomicBool>,
        }
        impl Preparation<()> for Restore {
            fn prepared(&self) -> &() {
                &()
            }
            fn into_prepared(self: Box<Self>) {}
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                assert!(self.sessions.upgrade().unwrap().try_lock().is_ok());
                self.restored.store(true, Ordering::Release);
            }
        }
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        let restored = Arc::new(AtomicBool::new(false));
        let mut expired = launch("expired", Role::Stream);
        expired.created = Instant::now() - Duration::from_secs(31);
        *expired.preparation.lock().unwrap() = Some(Box::new(Restore {
            sessions: Arc::downgrade(&sessions),
            restored: restored.clone(),
        }));
        let expired = {
            let mut locked = sessions.lock().unwrap();
            locked.pending.insert(expired.id.clone(), expired);
            let live = launch("live", Role::InputOnly);
            locked.queue(live.clone()).unwrap();
            assert_eq!(locked.pending_for_peer(live.peer).unwrap().id, "live");
            assert_eq!(locked.rtsp_for_peer(live.peer).len(), 1);
            let expired = locked.expire();
            assert_eq!(expired.len(), 1);
            assert_eq!(expired[0].id, "expired");
            assert!(locked.pending.contains_key("live"));
            assert!(!restored.load(Ordering::Acquire));
            expired
        };
        assert!(!restored.load(Ordering::Acquire));
        drop(expired);
        assert!(restored.load(Ordering::Acquire));
    }
    #[test]
    fn pending_launches_and_streams_keep_sessions_busy_until_they_expire_or_end() {
        let mut sessions = Sessions::default();
        assert!(sessions.idle());
        let mut pending = launch("pending", Role::Stream);
        sessions.pending.insert(pending.id.clone(), pending.clone());
        assert!(!sessions.idle());
        pending.created = Instant::now() - Duration::from_secs(31);
        sessions.pending.insert(pending.id.clone(), pending.clone());
        assert!(sessions.idle());
        assert!(sessions.owns_capture());
        pending.preparing.store(true, Ordering::Release);
        assert!(!sessions.idle());
        pending.preparing.store(false, Ordering::Release);
        let fresh = launch("fresh", Role::Stream);
        sessions.pending.insert(fresh.id.clone(), fresh.clone());
        sessions.start(fresh, Negotiated::default()).unwrap();
        assert!(!sessions.idle());
        sessions.active.clear();
        sessions.teardown.insert("ended".into(), Instant::now());
        assert!(!sessions.idle());
        sessions.teardown.clear();
        assert!(sessions.idle());
    }
    fn launch(id: &str, role: Role) -> Launch {
        Launch {
            id: id.into(),
            client: Client {
                name: "fixture".into(),
                cert: String::new(),
                uuid: "client".into(),
                perm: u32::MAX,
                enabled: true,
                extra: Default::default(),
            },
            peer: "127.0.0.1".parse().unwrap(),
            app_id: 1,
            key: [0; 16],
            key_id: 1,
            ping: "ping".into(),
            connect_data: 1,
            role,
            created: Instant::now(),
            rtsp_encrypted: true,
            rtsp_counter: Arc::new(AtomicU32::new(1)),
            rtsp_received: Default::default(),
            preparation: Default::default(),
            vrr_requested: false,
            host_audio: false,
            requested_rate: 0,
            options: Default::default(),
            audio_preparation: Default::default(),
            preparing: Default::default(),
            warnings: Default::default(),
        }
    }
    #[test]
    fn a_failed_stream_is_not_reported_as_a_normal_close() {
        let closed = Session::new(launch("closed", Role::Stream), Negotiated::default());
        closed.stop();
        assert_eq!(closed.termination_reason(), 0x8003_0023);
        let failed = Session::new(launch("failed", Role::Stream), Negotiated::default());
        failed.fail();
        assert!(failed.stopping());
        assert_eq!(failed.termination_reason(), 0x8000_4005);
    }
    #[test]
    fn a_worker_that_cannot_start_fails_without_leaving_an_active_session() {
        let mut sessions = Sessions::default();
        let failed = launch("failed", Role::Stream);
        sessions.queue(failed.clone()).unwrap();
        let failed = sessions.start(failed, Negotiated::default()).unwrap();
        let other = launch("other", Role::InputOnly);
        sessions.queue(other.clone()).unwrap();
        let other = sessions.start(other, Negotiated::default()).unwrap();
        sessions.fail_start(&failed);
        assert!(failed.failed());
        assert!(failed.stopping());
        assert_eq!(failed.termination_reason(), 0x8000_4005);
        assert!(!sessions.active.contains_key("failed"));
        assert!(sessions.active.contains_key("other"));
        assert!(!other.stopping());
        assert!(sessions.teardown.is_empty());
        let retry = launch("retry", Role::Stream);
        sessions.queue(retry.clone()).unwrap();
        sessions.start(retry, Negotiated::default()).unwrap();
    }
    #[test]
    fn teardown_leaves_active_while_resources_remain_owned() {
        use std::sync::{Mutex, Weak};
        struct Resource {
            sessions: Weak<Mutex<Sessions>>,
            released: Arc<AtomicBool>,
        }
        impl Preparation<()> for Resource {
            fn prepared(&self) -> &() {
                &()
            }
            fn into_prepared(self: Box<Self>) {}
        }
        impl Drop for Resource {
            fn drop(&mut self) {
                let sessions = self.sessions.upgrade().unwrap();
                let sessions = sessions.try_lock().unwrap();
                assert!(!sessions.active.contains_key("old"));
                assert!(sessions.teardown.contains_key("old"));
                self.released.store(true, Ordering::Release);
            }
        }
        let sessions = Arc::new(Mutex::new(Sessions::default()));
        let released = Arc::new(AtomicBool::new(false));
        let old = launch("old", Role::Stream);
        *old.preparation.lock().unwrap() = Some(Box::new(Resource {
            sessions: Arc::downgrade(&sessions),
            released: released.clone(),
        }));
        let old = {
            let mut sessions = sessions.lock().unwrap();
            sessions.queue(old.clone()).unwrap();
            sessions.start(old, Negotiated::default()).unwrap()
        };
        old.stop();
        {
            let mut sessions = sessions.lock().unwrap();
            sessions.begin_teardown(&old);
            assert!(sessions.active.is_empty());
            assert!(sessions.owns_capture());
            assert!(!sessions.idle());
            let mut next = launch("next", Role::Stream);
            next.app_id = 2;
            sessions.queue(next.clone()).unwrap();
            sessions.start(next, Negotiated::default()).unwrap();
            assert!(!released.load(Ordering::Acquire));
        }
        drop(old);
        assert!(released.load(Ordering::Acquire));
        let mut sessions = sessions.lock().unwrap();
        sessions.teardown.remove("old");
        sessions.active.clear();
        assert!(sessions.idle());
        assert!(!sessions.owns_capture());
    }
    #[test]
    fn only_pyrowave_sessions_report_quality_bitrates() {
        for codec in 0..=3 {
            let config = Negotiated {
                codec,
                rate_millihz: 59_940,
                ..Default::default()
            };
            let session = Session::new(launch("quality", Role::Stream), config.clone());
            let info = session.info();
            if codec == 3 {
                assert_eq!(
                    info["pyrowave_minimum_kbps"],
                    crate::pyrowave::minimum_kbps(config.width, config.height, 59_940)
                );
                assert_eq!(
                    info["pyrowave_recommended_kbps"],
                    crate::pyrowave::recommended_kbps(config.width, config.height, 59_940)
                );
            } else {
                assert!(info["pyrowave_minimum_kbps"].is_null());
                assert!(info["pyrowave_recommended_kbps"].is_null());
            }
            session.bitrate.store(100_000, Ordering::Relaxed);
            assert_eq!(session.info()["encoder_bitrate_kbps"], 100_000);
        }
    }
    #[test]
    fn pyrowave_feedback_cannot_force_frames_or_turn_invalidation_into_idr() {
        let session = Session::new(
            launch("pyrowave", Role::Stream),
            Negotiated {
                codec: 3,
                ..Default::default()
            },
        );
        session.idr.store(false, Ordering::Release);
        *session.recovery_wake.lock().unwrap() = Some(Box::new(|| {
            panic!("intra-only feedback must not wake the session");
        }));
        for (first, last) in [(1, 2), (0, 0), (4, 3)] {
            session.request_invalidation(first, last);
        }
        assert_eq!(
            session
                .stats
                .reference_invalidations
                .load(Ordering::Relaxed),
            3
        );
        assert_eq!(session.stats.idr_requests.load(Ordering::Relaxed), 0);
        assert!(session.invalidation.lock().unwrap().is_none());
        session.request_idr();
        assert_eq!(session.stats.idr_requests.load(Ordering::Relaxed), 1);
        assert!(!session.idr.load(Ordering::Acquire));
    }
    #[test]
    fn send_loss_recovery_forces_a_keyframe_without_counting_a_client_request() {
        for codec in 0..=3 {
            let session = Session::new(
                launch("send-loss", Role::Stream),
                Negotiated {
                    codec,
                    ..Default::default()
                },
            );
            session.idr.store(false, Ordering::Release);
            session.request_send_loss_recovery();
            // PyroWave frames are all intra; nothing to force.
            assert_eq!(session.idr.load(Ordering::Acquire), codec != 3);
            assert_eq!(session.stats.idr_requests.load(Ordering::Relaxed), 0);
            assert_eq!(session.info()["send_loss_recoveries"], 1);
        }
    }
    #[test]
    fn inter_frame_recovery_keeps_invalidation_ranges_and_idr_fallback() {
        for codec in 0..=2 {
            let session = Session::new(
                launch("inter-frame", Role::Stream),
                Negotiated {
                    codec,
                    ..Default::default()
                },
            );
            session.idr.store(false, Ordering::Release);
            session.request_invalidation(4, 6);
            session.request_invalidation(2, 3);
            assert_eq!(*session.invalidation.lock().unwrap(), Some((2, 6)));
            assert!(!session.idr.load(Ordering::Acquire));
            session.request_invalidation(0, 0);
            assert!(session.idr.load(Ordering::Acquire));
            assert_eq!(session.stats.idr_requests.load(Ordering::Relaxed), 1);
            assert_eq!(
                session
                    .stats
                    .reference_invalidations
                    .load(Ordering::Relaxed),
                3
            );
        }
    }
    #[test]
    fn independent_roles_and_targeted_teardown_preserve_other_sessions() {
        let mut sessions = Sessions::default();
        sessions.queue(launch("game", Role::Stream)).unwrap();
        sessions
            .queue(launch("monitor", Role::RemoteMonitor))
            .unwrap();
        sessions.queue(launch("input", Role::InputOnly)).unwrap();
        // A client launching again replaces its own earlier launch in that role.
        sessions
            .queue(launch("duplicate", Role::RemoteMonitor))
            .unwrap();
        assert!(!sessions.pending.contains_key("monitor"));
        assert!(sessions.pending.contains_key("duplicate"));
        let game = sessions
            .start(launch("game", Role::Stream), Negotiated::default())
            .unwrap();
        let input = sessions
            .start(launch("input", Role::InputOnly), Negotiated::default())
            .unwrap();
        sessions.stop_role(Role::RemoteMonitor, Some("client"));
        assert!(sessions.pending.is_empty());
        assert!(!game.stopping());
        assert!(!input.stopping());
        sessions.stop_role(Role::Stream, None);
        assert!(game.stopping());
        assert!(!input.stopping());
        sessions.request_stop(Some("client"));
        assert!(input.stopping());
    }
    #[test]
    fn a_new_launch_replaces_an_unconnected_launch_and_stops_an_abandoned_stream() {
        let mut sessions = Sessions::default();
        // A launch whose RTSP never arrived must not block the next attempt.
        sessions.queue(launch("first", Role::Stream)).unwrap();
        sessions.queue(launch("retry", Role::Stream)).unwrap();
        assert_eq!(sessions.pending.keys().collect::<Vec<_>>(), ["retry"]);
        // A stream the client abandoned is stopped when it launches again.
        let old = sessions
            .start(launch("retry", Role::Stream), Negotiated::default())
            .unwrap();
        sessions.queue(launch("resume", Role::Stream)).unwrap();
        assert!(old.stopping());
        assert!(sessions.pending.contains_key("resume"));
        // Another client's stream is untouched.
        let mut other = launch("other", Role::Stream);
        other.client.uuid = "other-client".into();
        sessions.queue(other.clone()).unwrap();
        let other = sessions.start(other, Negotiated::default()).unwrap();
        sessions.queue(launch("again", Role::Stream)).unwrap();
        assert!(!other.stopping());
    }
}
/// A platform preparation that can be borrowed or moved into its stream.
pub trait Preparation<T>: Send {
    fn prepared(&self) -> &T;
    fn into_prepared(self: Box<Self>) -> T;
}

/// A prepared audio route shared with the app and its stream.
pub trait AudioPreparation<T>: Send + Sync {
    fn route(&self) -> Arc<T>;
}

pub struct Launch<P = (), A = ()> {
    pub warnings: Arc<Warnings>,
    pub id: String,
    pub client: Client,
    pub peer: IpAddr,
    pub app_id: u32,
    pub key: [u8; 16],
    pub key_id: u32,
    pub ping: String,
    pub connect_data: u32,
    pub role: Role,
    pub created: Instant,
    pub rtsp_encrypted: bool,
    pub rtsp_counter: Arc<AtomicU32>,
    pub rtsp_received: Arc<std::sync::Mutex<crate::packet::ReplayWindow>>,
    /// The platform host owns launch preparation. Keeping it on the launch
    /// gives expired, rejected and disconnected requests the same RAII teardown.
    pub preparation: Arc<std::sync::Mutex<Option<Box<dyn Preparation<P>>>>>,
    pub vrr_requested: bool,
    pub host_audio: bool,
    pub requested_rate: u32,
    pub options: BTreeMap<String, String>,
    pub audio_preparation: Arc<std::sync::Mutex<Option<Box<dyn AudioPreparation<A>>>>>,
    /// While the host still prepares the launch (displays, audio, the app's
    /// own commands, which can take minutes), it does not expire: the client
    /// connects only after the launch reply.
    pub preparing: Arc<AtomicBool>,
}
// Launch clones share the slots; the platform preparations need not be Clone.
impl<P, A> Clone for Launch<P, A> {
    fn clone(&self) -> Self {
        Self {
            warnings: self.warnings.clone(),
            id: self.id.clone(),
            client: self.client.clone(),
            peer: self.peer,
            app_id: self.app_id,
            key: self.key,
            key_id: self.key_id,
            ping: self.ping.clone(),
            connect_data: self.connect_data,
            role: self.role,
            created: self.created,
            rtsp_encrypted: self.rtsp_encrypted,
            rtsp_counter: self.rtsp_counter.clone(),
            rtsp_received: self.rtsp_received.clone(),
            preparation: self.preparation.clone(),
            vrr_requested: self.vrr_requested,
            host_audio: self.host_audio,
            requested_rate: self.requested_rate,
            options: self.options.clone(),
            audio_preparation: self.audio_preparation.clone(),
            preparing: self.preparing.clone(),
        }
    }
}
impl<P, A> Launch<P, A> {
    /// Still waiting for its client: being prepared, or prepared under 30 s ago.
    fn live(&self) -> bool {
        self.preparing.load(Ordering::Acquire) || self.created.elapsed() < Duration::from_secs(30)
    }
}
#[derive(Default)]
pub struct Stats {
    pub frames: AtomicU64,
    pub packets: AtomicU64,
    pub bytes: AtomicU64,
    pub idr_requests: AtomicU64,
    pub reference_invalidations: AtomicU64,
    /// Keyframes the host started itself after it failed to send a frame
    /// that FEC can repair; not counted in `idr_requests`.
    pub send_loss_recoveries: AtomicU64,
    pub latency_us: AtomicU64,
    pub frames_replaced: AtomicU64,
    /// Last frame offered to the video socket, plus one; zero means none yet.
    pub video_frame: AtomicU64,
    pub performance: std::sync::Mutex<crate::performance::Performance>,
}
pub struct Session<P = (), A = ()> {
    pub encoder: std::sync::RwLock<String>,
    pub capture_warnings: std::sync::RwLock<Arc<Warnings>>,
    pub launch: Launch<P, A>,
    pub config: Negotiated,
    pub stop: AtomicBool,
    /// Set before `stop` when the stream ends on an error, so the client is
    /// told it failed rather than that the host closed it.
    pub failed: AtomicBool,
    pub idr: AtomicBool,
    pub invalidation: std::sync::Mutex<Option<(u64, u64)>>,
    pub recovery_wake: std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    pub bitrate: AtomicU32,
    pub stats: Stats,
    pub started: Instant,
    pub output: std::sync::RwLock<String>,
    /// The display's HDR metadata, once the encoder has it. Clients are told
    /// the HDR state only then, so they hear it once with the real values.
    pub hdr_metadata: std::sync::RwLock<Option<crate::hdr::Metadata>>,
}
impl<P, A> Session<P, A> {
    pub fn new(launch: Launch<P, A>, config: Negotiated) -> Arc<Self> {
        let bitrate = config.bitrate_kbps;
        Arc::new(Self {
            encoder: Default::default(),
            capture_warnings: Default::default(),
            launch,
            config,
            stop: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            idr: AtomicBool::new(true),
            invalidation: Default::default(),
            recovery_wake: Default::default(),
            bitrate: AtomicU32::new(bitrate),
            stats: Stats::default(),
            started: Instant::now(),
            output: std::sync::RwLock::new(String::new()),
            hdr_metadata: Default::default(),
        })
    }
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release)
    }
    pub fn fail(&self) {
        self.failed.store(true, Ordering::Release);
        self.stop();
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    /// The termination reason the control stream sends. Moonlight closes
    /// quietly on 0x80030023 (the host closed the stream) and shows an error
    /// with the code otherwise; 0x80004005 is the generic failure HRESULT.
    pub fn termination_reason(&self) -> u32 {
        if self.failed() {
            0x8000_4005
        } else {
            0x8003_0023
        }
    }
    pub fn request_idr(&self) {
        self.stats.idr_requests.fetch_add(1, Ordering::Relaxed);
        // Intra-only frames already recover; feedback must not bypass cadence.
        if self.config.codec != 3 {
            self.idr.store(true, Ordering::Release);
            self.wake_recovery();
        }
    }
    /// The host could not send enough of a frame for the client to rebuild
    /// it, so the next frame is a keyframe. Moonlight, waiting for one,
    /// then recovers on that frame and sends no request of its own.
    pub fn request_send_loss_recovery(&self) {
        self.stats
            .send_loss_recoveries
            .fetch_add(1, Ordering::Relaxed);
        if self.config.codec != 3 {
            self.idr.store(true, Ordering::Release);
        }
    }
    pub fn request_invalidation(&self, first: u64, last: u64) {
        self.stats
            .reference_invalidations
            .fetch_add(1, Ordering::Relaxed);
        if self.config.codec == 3 {
            return;
        }
        if first == 0 || first > last {
            self.request_idr();
            return;
        }
        let mut pending = self.invalidation.lock().unwrap();
        *pending = Some(pending.map_or((first, last), |(a, b)| (a.min(first), b.max(last))));
        drop(pending);
        self.wake_recovery();
    }
    fn wake_recovery(&self) {
        if let Some(wake) = self.recovery_wake.lock().unwrap().as_ref() {
            wake();
        }
    }
    pub fn record_fec_status(&self, payload: &[u8]) {
        let last_sent = self
            .stats
            .video_frame
            .load(Ordering::Acquire)
            .checked_sub(1)
            .map(|v| v as u32);
        self.stats
            .performance
            .lock()
            .unwrap()
            .record_fec_status(payload, last_sent);
    }
    pub fn info(&self) -> serde_json::Value {
        let mut warnings = self.launch.warnings.snapshot();
        warnings.extend(self.capture_warnings.read().unwrap().snapshot());
        serde_json::json!({"warnings":warnings,"encoder":*self.encoder.read().unwrap(),"uuid":self.launch.client.uuid,"device_name":self.launch.client.name,"width":self.config.width,"height":self.config.height,"fps":self.config.fps,"video_format":self.config.codec,"hdr":self.config.hdr,"vrr":self.config.vrr_low_latency,"encoder_bitrate_kbps":self.bitrate.load(Ordering::Relaxed),"pyrowave_minimum_kbps":(self.config.codec == 3).then(|| crate::pyrowave::minimum_kbps(self.config.width, self.config.height, self.config.fps_millihz())),"pyrowave_recommended_kbps":(self.config.codec == 3).then(|| crate::pyrowave::recommended_kbps(self.config.width, self.config.height, self.config.fps_millihz())),"audio_channels":self.config.audio_channels,"state":if self.stopping(){"STOPPING"}else{"RUNNING"},"frames_sent":self.stats.frames.load(Ordering::Relaxed),"frames_replaced":self.stats.frames_replaced.load(Ordering::Relaxed),"packets_sent":self.stats.packets.load(Ordering::Relaxed),"bytes_sent":self.stats.bytes.load(Ordering::Relaxed),"idr_requests":self.stats.idr_requests.load(Ordering::Relaxed),"reference_invalidations":self.stats.reference_invalidations.load(Ordering::Relaxed),"send_loss_recoveries":self.stats.send_loss_recoveries.load(Ordering::Relaxed),"encode_latency_ms":self.stats.latency_us.load(Ordering::Relaxed) as f64/1000.,"performance":self.stats.performance.lock().unwrap().snapshot(Instant::now()),"uptime_seconds":self.started.elapsed().as_secs_f64(),"role":self.launch.role})
    }
}
pub struct Sessions<P = (), A = ()> {
    pub pending: BTreeMap<String, Launch<P, A>>,
    pub active: BTreeMap<String, Arc<Session<P, A>>>,
    pub teardown: BTreeMap<String, Instant>,
}
impl<P, A> Default for Sessions<P, A> {
    fn default() -> Self {
        Self {
            pending: BTreeMap::new(),
            active: BTreeMap::new(),
            teardown: BTreeMap::new(),
        }
    }
}
impl<P, A> Sessions<P, A> {
    #[must_use = "drop expired launches after releasing the sessions lock"]
    pub fn expire(&mut self) -> Vec<Launch<P, A>> {
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| !p.live())
            .map(|(id, _)| id.clone())
            .collect();
        expired
            .iter()
            .filter_map(|id| self.pending.remove(id))
            .collect()
    }
    /// Withdraw a client's launches and streams in one role. Moonlight starts a
    /// stream only after abandoning its previous one, which may never have
    /// connected or may not have timed out yet; either would otherwise block
    /// the new attempt. Returns the streams asked to stop.
    pub fn supersede(&mut self, client: &str, role: Role) -> Vec<Arc<Session<P, A>>> {
        self.pending
            .retain(|_, p| p.client.uuid != client || p.role != role);
        let stopped: Vec<_> = self
            .active
            .values()
            .filter(|s| s.launch.client.uuid == client && s.launch.role == role)
            .cloned()
            .collect();
        for session in &stopped {
            session.stop();
        }
        stopped
    }
    /// Queue a launch, replacing the same client's earlier launch in its role.
    pub fn queue(&mut self, launch: Launch<P, A>) -> Result<()> {
        self.supersede(&launch.client.uuid, launch.role);
        let streaming = self.active.values().filter(|s| !s.stopping()).count();
        if self.pending.values().filter(|p| p.live()).count() + streaming >= 16 {
            bail!("session limit reached");
        }
        self.pending.insert(launch.id.clone(), launch);
        Ok(())
    }
    pub fn pending_for_peer(&self, peer: IpAddr) -> Result<Launch<P, A>> {
        let mut found = self.pending.values().filter(|p| p.peer == peer && p.live());
        let p = found.next().cloned();
        if found.next().is_some() {
            bail!("ambiguous pending session identity");
        }
        p.ok_or_else(|| anyhow::anyhow!("no authorized launch for RTSP peer"))
    }
    pub fn rtsp_for_peer(&self, peer: IpAddr) -> Vec<Launch<P, A>> {
        self.pending
            .values()
            .filter(|p| p.live())
            .chain(
                self.active
                    .values()
                    .filter(|s| !s.stopping())
                    .map(|s| &s.launch),
            )
            .filter(|p| p.peer == peer)
            .cloned()
            .collect()
    }
    pub fn start(
        &mut self,
        launch: Launch<P, A>,
        config: Negotiated,
    ) -> Result<Arc<Session<P, A>>> {
        if self.pending.remove(&launch.id).is_none() {
            bail!("launch expired or already consumed");
        }
        let s = Session::new(launch, config);
        self.active.insert(s.launch.id.clone(), s.clone());
        Ok(s)
    }
    pub fn begin_teardown(&mut self, session: &Session<P, A>) {
        self.active.remove(&session.launch.id);
        self.teardown
            .insert(session.launch.id.clone(), Instant::now());
    }
    pub fn fail_start(&mut self, session: &Session<P, A>) {
        session.fail();
        self.active.remove(&session.launch.id);
    }
    pub fn request_stop(&mut self, id: Option<&str>) {
        self.pending
            .retain(|_, p| id.is_some_and(|id| p.client.uuid != id && p.id != id));
        for s in self.active.values() {
            if id.is_none_or(|id| s.launch.client.uuid == id || s.launch.id == id) {
                s.stop();
            }
        }
    }
    pub fn owns_capture(&self) -> bool {
        !self.pending.is_empty() || !self.active.is_empty() || !self.teardown.is_empty()
    }
    /// No stream runs, tears down or is about to start. Unlike
    /// `owns_capture`, an expired launch that nothing has removed yet, such
    /// as one the client abandoned, does not count.
    pub fn idle(&self) -> bool {
        self.active.is_empty()
            && self.teardown.is_empty()
            && !self.pending.values().any(Launch::live)
    }
    pub fn stop_role(&mut self, role: Role, client: Option<&str>) {
        self.pending
            .retain(|_, p| p.role != role || client.is_some_and(|id| p.client.uuid != id));
        for s in self.active.values() {
            if s.launch.role == role && client.is_none_or(|id| id == s.launch.client.uuid) {
                s.stop();
            }
        }
    }
}
