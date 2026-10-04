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

#[cfg(test)]
mod tests {
    use super::*;
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
#[derive(Clone)]
pub struct Launch {
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
    pub preparation: Arc<std::sync::Mutex<Option<Box<dyn std::any::Any + Send>>>>,
    pub vrr_requested: bool,
    pub host_audio: bool,
    pub requested_rate: u32,
    pub options: BTreeMap<String, String>,
    pub audio_preparation: Arc<std::sync::Mutex<Option<Box<dyn std::any::Any + Send + Sync>>>>,
}
#[derive(Default)]
pub struct Stats {
    pub frames: AtomicU64,
    pub packets: AtomicU64,
    pub bytes: AtomicU64,
    pub idr_requests: AtomicU64,
    pub latency_us: AtomicU64,
    pub frames_replaced: AtomicU64,
    pub performance: std::sync::Mutex<crate::performance::Performance>,
}
pub struct Session {
    pub launch: Launch,
    pub config: Negotiated,
    pub stop: AtomicBool,
    /// Set before `stop` when the stream ends on an error, so the client is
    /// told it failed rather than that the host closed it.
    pub failed: AtomicBool,
    pub idr: AtomicBool,
    pub invalidation: std::sync::Mutex<Option<(u64, u64)>>,
    pub bitrate: AtomicU32,
    pub stats: Stats,
    pub started: Instant,
    pub output: std::sync::RwLock<String>,
    pub hdr_metadata: std::sync::RwLock<crate::hdr::Metadata>,
}
impl Session {
    pub fn new(launch: Launch, config: Negotiated) -> Arc<Self> {
        let bitrate = config.bitrate_kbps;
        Arc::new(Self {
            launch,
            config,
            stop: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            idr: AtomicBool::new(true),
            invalidation: Default::default(),
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
        self.idr.store(true, Ordering::Release);
        self.stats.idr_requests.fetch_add(1, Ordering::Relaxed);
    }
    pub fn request_invalidation(&self, first: u64, last: u64) {
        if first == 0 || first > last {
            self.request_idr();
            return;
        }
        let mut pending = self.invalidation.lock().unwrap();
        *pending = Some(pending.map_or((first, last), |(a, b)| (a.min(first), b.max(last))));
    }
    pub fn info(&self) -> serde_json::Value {
        serde_json::json!({"uuid":self.launch.client.uuid,"device_name":self.launch.client.name,"width":self.config.width,"height":self.config.height,"fps":self.config.fps,"video_format":self.config.codec,"hdr":self.config.hdr,"vrr":self.config.vrr_low_latency,"encoder_bitrate_kbps":self.bitrate.load(Ordering::Relaxed),"audio_channels":self.config.audio_channels,"state":if self.stopping(){"STOPPING"}else{"RUNNING"},"frames_sent":self.stats.frames.load(Ordering::Relaxed),"frames_replaced":self.stats.frames_replaced.load(Ordering::Relaxed),"packets_sent":self.stats.packets.load(Ordering::Relaxed),"bytes_sent":self.stats.bytes.load(Ordering::Relaxed),"idr_requests":self.stats.idr_requests.load(Ordering::Relaxed),"encode_latency_ms":self.stats.latency_us.load(Ordering::Relaxed) as f64/1000.,"performance":self.stats.performance.lock().unwrap().snapshot(Instant::now()),"uptime_seconds":self.started.elapsed().as_secs_f64(),"role":self.launch.role})
    }
}
#[derive(Default)]
pub struct Sessions {
    pub pending: BTreeMap<String, Launch>,
    pub active: BTreeMap<String, Arc<Session>>,
    pub teardown: usize,
}
impl Sessions {
    pub fn expire(&mut self) {
        self.pending
            .retain(|_, p| p.created.elapsed() < Duration::from_secs(30));
    }
    /// Withdraw a client's launches and streams in one role. Moonlight starts a
    /// stream only after abandoning its previous one, which may never have
    /// connected or may not have timed out yet; either would otherwise block
    /// the new attempt. Returns the streams asked to stop.
    pub fn supersede(&mut self, client: &str, role: Role) -> Vec<Arc<Session>> {
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
    pub fn queue(&mut self, launch: Launch) -> Result<()> {
        self.expire();
        self.supersede(&launch.client.uuid, launch.role);
        let streaming = self.active.values().filter(|s| !s.stopping()).count();
        if self.pending.len() + streaming >= 16 {
            bail!("session limit reached");
        }
        self.pending.insert(launch.id.clone(), launch);
        Ok(())
    }
    pub fn pending_for_peer(&mut self, peer: IpAddr) -> Result<Launch> {
        self.expire();
        let mut found = self.pending.values().filter(|p| p.peer == peer);
        let p = found.next().cloned();
        if found.next().is_some() {
            bail!("ambiguous pending session identity");
        }
        p.ok_or_else(|| anyhow::anyhow!("no authorized launch for RTSP peer"))
    }
    pub fn rtsp_for_peer(&mut self, peer: IpAddr) -> Vec<Launch> {
        self.expire();
        self.pending
            .values()
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
    pub fn start(&mut self, launch: Launch, config: Negotiated) -> Result<Arc<Session>> {
        if self.pending.remove(&launch.id).is_none() {
            bail!("launch expired or already consumed");
        }
        let s = Session::new(launch, config);
        self.active.insert(s.launch.id.clone(), s.clone());
        Ok(s)
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
        !self.pending.is_empty() || !self.active.is_empty() || self.teardown != 0
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
