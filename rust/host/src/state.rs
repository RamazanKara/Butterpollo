use anyhow::{Context, Result};
use butterpollo_core::{
    config::Config,
    crypto::Identity,
    pairing::Pairings,
    session::Warnings,
    state::{App, Credentials, PairedState, ProfileFiles},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};
pub type Shared = Arc<Host>;
#[cfg(test)]
pub(crate) mod test_support;
pub type Launch = butterpollo_core::session::Launch<
    crate::display_session::StreamPreparation,
    butterpollo_windows::audio_route::Route,
>;
pub type Session = butterpollo_core::session::Session<
    crate::display_session::StreamPreparation,
    butterpollo_windows::audio_route::Route,
>;
pub type Sessions = butterpollo_core::session::Sessions<
    crate::display_session::StreamPreparation,
    butterpollo_windows::audio_route::Route,
>;
use crate::web_sessions::{self, WebSession};

const STANDARD_CODECS: u32 = 0x1 | 0x100 | 0x200 | 0x10000 | 0x20000;
/// Bits from the separate PyroWave (Vulkan) probe.
const PYROWAVE_CODECS: u32 = 0x0780_0000;
const CODEC_RETRY_MIN: Duration = Duration::from_secs(5);
const CODEC_RETRY_MAX: Duration = Duration::from_secs(60);

struct CodecRetry {
    delay: Duration,
    next: Option<Instant>,
    on_demand: Instant,
    warning: Option<Instant>,
}
impl CodecRetry {
    fn new(now: Instant) -> Self {
        Self {
            delay: CODEC_RETRY_MIN,
            next: None,
            on_demand: now,
            warning: None,
        }
    }
    fn completed(
        &mut self,
        flags: u32,
        error: Option<&str>,
        warnings: &Warnings,
        now: Instant,
    ) -> bool {
        if flags & !0x40000000 == 0 {
            if self.warn(now) {
                // Clearing makes an unchanged warning log its next reminder.
                warnings.clear("video_encoder");
                warnings.set(
                    "video_encoder",
                    format!(
                        "No video encoder available: {}. Retrying.",
                        error.unwrap_or("the encoder produced no packets")
                    ),
                );
            }
        } else {
            warnings.clear("video_encoder");
            self.warning = None;
        }
        if flags & STANDARD_CODECS != 0 {
            self.next = None;
            self.delay = CODEC_RETRY_MIN;
            return false;
        }
        self.next = Some(now + self.delay);
        self.on_demand = now + CODEC_RETRY_MIN;
        self.delay = (self.delay * 2).min(CODEC_RETRY_MAX);
        true
    }
    fn due(&self, now: Instant, requested: bool) -> bool {
        self.next
            .is_some_and(|next| now >= next || (requested && now >= self.on_demand))
    }
    fn warn(&mut self, now: Instant) -> bool {
        if self.warning.is_some_and(|next| now < next) {
            return false;
        }
        self.warning = Some(now + CODEC_RETRY_MAX);
        true
    }
}
/// A one-time PIN that lets a client pair with a passphrase, valid 180 s.
pub struct OneTimePin {
    pub pin: String,
    pub passphrase: String,
    pub device_name: String,
    pub created: Instant,
}
pub struct PendingPin {
    pub name: String,
    /// The requesting device's certificate: Moonlight's apps share one
    /// unique ID, so this tells another device apart.
    pub certificate: String,
    pub created: Instant,
    pub sender: tokio::sync::oneshot::Sender<(String, String)>,
}
/// A client's display lease and when it last stopped being streamed.
pub type RetainedDisplay = (Arc<crate::display_session::Ready>, Option<Instant>);
/// The display a stream launch resumes: only this client's own retained
/// display, and only in the mode it asks for. Reuse leaves the entry (and the
/// display) untouched; a retained display in another mode is removed and
/// handed back so the caller drops it, restoring Windows, outside the lock.
pub fn take_retained<L: Clone>(
    displays: &mut BTreeMap<String, (L, Option<Instant>)>,
    client: &str,
    matches: impl FnOnce(&L) -> bool,
) -> Result<L, Option<L>> {
    match displays.get(client) {
        Some((lease, _)) if matches(lease) => Ok(lease.clone()),
        Some(_) => Err(displays.remove(client).map(|(lease, _)| lease)),
        None => Err(None),
    }
}
pub struct Host {
    #[cfg(test)]
    pub reconnect_fixture: Option<Arc<crate::stream::reconnect_tests::Fixture>>,
    pub directory: PathBuf,
    pub config_path: PathBuf,
    pub paired_path: PathBuf,
    pub credentials_path: PathBuf,
    pub apps_path: PathBuf,
    pub aliases_path: PathBuf,
    pub aliases: Mutex<Value>,
    pub config: RwLock<Config>,
    pub identity: Identity,
    pub paired: RwLock<PairedState>,
    pub credentials: RwLock<Option<Credentials>>,
    pub app_document: RwLock<Value>,
    pub apps: RwLock<Vec<App>>,
    pub sessions: Mutex<Sessions>,
    pub pairings: Mutex<Pairings>,
    pub pins: Mutex<BTreeMap<String, PendingPin>>,
    pub otp: Mutex<Option<OneTimePin>>,
    pub web_sessions: Mutex<HashMap<String, WebSession>>,
    pub stop: std::sync::atomic::AtomicBool,
    pub restart: std::sync::atomic::AtomicBool,
    pub codecs: std::sync::atomic::AtomicU32,
    pub probing_codecs: std::sync::atomic::AtomicBool,
    codec_probe_requested: std::sync::atomic::AtomicBool,
    pub warnings: Warnings,
    video_codecs_ready: tokio::sync::watch::Sender<bool>,
    pub current_app: Mutex<Option<crate::process::RunningApp>>,
    pub live_rtx: Mutex<Option<(String, serde_json::Map<String, Value>)>>,
    pub launch_transition: Mutex<()>,
    pub confirmations: Mutex<butterpollo_core::remote::Confirmations>,
    pub app_audio: Mutex<Option<Arc<butterpollo_windows::audio_route::Route>>>,
    /// Each client's game display, kept between its streams while the app runs.
    pub app_display: Mutex<BTreeMap<String, RetainedDisplay>>,
    pub monitors: Mutex<BTreeMap<String, Arc<butterpollo_windows::display::Retained>>>,
    pub updates: Mutex<Value>,
    pub metadata: Mutex<Option<(Instant, Value)>>,
    /// The encoder family the capability probe selected for H.264.
    pub probed_encoder: Mutex<&'static str>,
    pub assets: PathBuf,
}
impl Host {
    pub fn stop_app(&self) {
        // Stopping the app waits for it to exit, up to its exit timeout and
        // undo commands: never under the lock every serverinfo request takes.
        let app = self.current_app.lock().unwrap().take();
        drop(app);
        self.live_rtx.lock().unwrap().take();
        self.app_audio.lock().unwrap().take();
        self.app_display.lock().unwrap().clear();
    }
    pub fn reap_paused_display(&self) {
        let config = self.config.read().unwrap().clone();
        let delay = if config.boolean("dd_config_revert_on_disconnect", false) {
            Duration::from_millis(config.integer("dd_config_revert_delay", 3000).max(0) as u64)
        } else {
            let timeout = config
                .integer("dd_paused_virtual_display_timeout_secs", 7200)
                .max(0);
            if timeout == 0 {
                return;
            }
            Duration::from_secs(timeout as u64)
        };
        let released: Vec<_> = {
            let mut displays = self.app_display.lock().unwrap();
            let mut expired = vec![];
            for (owner, (lease, paused)) in displays.iter_mut() {
                if Arc::strong_count(lease) > 1 {
                    *paused = None;
                } else if paused.get_or_insert_with(Instant::now).elapsed() >= delay {
                    expired.push(owner.clone());
                }
            }
            expired
                .iter()
                .filter_map(|owner| displays.remove(owner))
                .collect()
        };
        // Display leases restore Windows settings; do that outside the lock.
        drop(released);
    }
    pub fn load(directory: PathBuf, assets: PathBuf, port: Option<u16>) -> Result<Shared> {
        std::fs::create_dir_all(&directory)?;
        let config_path = directory.join("sunshine.conf");
        let mut config = Config::load(&config_path)?;
        if let Some(port) = port {
            config.values.insert("port".into(), port.to_string());
        }
        config.ports()?;
        let ProfileFiles {
            paired: paired_path,
            credentials: credentials_path,
            apps: apps_path,
            aliases: aliases_path,
            certificate,
            key,
        } = ProfileFiles::new(&config, &directory);
        let mut aliases = butterpollo_core::state::load_json(&aliases_path, json!({"root":{}}))?;
        let mut paired = PairedState::load(&paired_path)?;
        // Vibepollo keeps the shared virtual display's GUID in
        // vibeshine_state.json; reuse it so Windows keeps that display's
        // settings.
        if paired.document["root"]["shared_virtual_display_guid"]
            .as_str()
            .is_none()
            && let Some(id) = aliases["root"]["shared_virtual_display_guid"]
                .as_str()
                .filter(|id| uuid::Uuid::parse_str(id).is_ok())
        {
            paired.document["root"]["shared_virtual_display_guid"] = id.into();
        }
        paired.save(&paired_path)?;
        let identity = Identity::load(&certificate, &key)?;
        let credentials = Credentials::load(&credentials_path)?;
        let app_document = load_library(
            &apps_path,
            // Vibepollo's default library.
            json!({"env":{},"apps":[
                {"name":"Desktop","image-path":"desktop.png","allow-client-commands":false},
                {"name":"Steam Big Picture","prep-cmd":[{"do":"","undo":"steam://close/bigpicture","elevated":false}],
                 "detached":["steam://open/bigpicture"],"image-path":"steam.png"}
            ]}),
        )?;
        let mut app_document = app_document;
        let mut assigned = false;
        if let Some(list) = app_document.get_mut("apps").and_then(Value::as_array_mut) {
            for app in list.iter_mut().filter_map(Value::as_object_mut) {
                if app
                    .get("uuid")
                    .and_then(Value::as_str)
                    .is_none_or(|uuid| uuid.trim().is_empty())
                {
                    app.insert("uuid".into(), json!(uuid::Uuid::new_v4().to_string()));
                    assigned = true;
                }
                // A ViGEmBus controller type, say, becomes its VHF pad.
                assigned |= butterpollo_core::config::replace_retired_in_app(app);
            }
        }
        if assigned {
            butterpollo_core::state::write_json(&apps_path, &app_document)?;
        }
        let mut apps: Vec<App> =
            serde_json::from_value(app_document.get("apps").cloned().unwrap_or(json!([])))?;
        butterpollo_core::catalog::assign(
            &mut apps,
            &mut aliases,
            assets.parent().unwrap_or(&assets),
        )?;
        butterpollo_core::state::write_json(&aliases_path, &aliases)?;
        let web_sessions = web_sessions::load(
            &aliases,
            credentials
                .as_ref()
                .map(|c| c.username.as_str())
                .unwrap_or(""),
        );
        Ok(Arc::new(Self {
            #[cfg(test)]
            reconnect_fixture: None,
            directory,
            config_path,
            paired_path,
            credentials_path,
            apps_path,
            aliases_path,
            aliases: Mutex::new(aliases),
            config: RwLock::new(config),
            identity,
            paired: RwLock::new(paired),
            credentials: RwLock::new(credentials),
            app_document: RwLock::new(app_document),
            apps: RwLock::new(apps),
            sessions: Mutex::new(Sessions::default()),
            pairings: Mutex::new(Pairings::default()),
            pins: Mutex::new(BTreeMap::new()),
            otp: Mutex::new(None),
            web_sessions: Mutex::new(web_sessions),
            stop: std::sync::atomic::AtomicBool::new(false),
            restart: std::sync::atomic::AtomicBool::new(false),
            codecs: std::sync::atomic::AtomicU32::new(0),
            probing_codecs: std::sync::atomic::AtomicBool::new(true),
            codec_probe_requested: std::sync::atomic::AtomicBool::new(false),
            warnings: Warnings::default(),
            video_codecs_ready: tokio::sync::watch::channel(false).0,
            current_app: Mutex::new(None),
            live_rtx: Default::default(),
            launch_transition: Mutex::new(()),
            confirmations: Mutex::new(Default::default()),
            app_audio: Mutex::new(None),
            app_display: Mutex::new(BTreeMap::new()),
            monitors: Mutex::new(BTreeMap::new()),
            updates: Mutex::new(
                json!({"status":true,"checking":false,"check_failed":false,"checked_at":0,"releases":[]}),
            ),
            assets,
            metadata: Mutex::new(None),
            probed_encoder: Mutex::new(""),
        }))
    }
    pub fn assign_apps(&self, apps: &mut [App]) -> Result<()> {
        let mut aliases = self.aliases.lock().unwrap();
        let mut next = aliases.clone();
        butterpollo_core::catalog::assign(
            apps,
            &mut next,
            self.assets.parent().unwrap_or(&self.assets),
        )?;
        butterpollo_core::state::write_json(&self.aliases_path, &next)?;
        *aliases = next;
        Ok(())
    }
    pub fn update_live_rtx(
        &self,
        uuid: &str,
        values: &serde_json::Map<String, Value>,
    ) -> Result<bool> {
        if uuid.is_empty() {
            anyhow::bail!("application UUID required");
        }
        let values: serde_json::Map<_, _> = values
            .iter()
            .filter(|(key, _)| crate::stream::RTX_KEYS.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let mut validation = self.config.read().unwrap().clone();
        validation.update(&values)?;
        let current = self.current_app.lock().unwrap();
        if !current.as_ref().is_some_and(|app| app.uuid == uuid) {
            return Ok(false);
        }
        let mut live = self.live_rtx.lock().unwrap();
        let next = Some((uuid.to_owned(), values));
        let changed = *live != next;
        *live = next;
        Ok(changed)
    }
    pub async fn wait_for_video_codecs(&self) {
        #[cfg(test)]
        if self.reconnect_fixture.is_some() {
            return;
        }
        let mut ready = self.video_codecs_ready.subscribe();
        // A stuck vendor probe must not hold /serverinfo or /applist open forever.
        let _ = tokio::time::timeout(Duration::from_secs(10), ready.wait_for(|ready| *ready)).await;
    }
    pub fn request_codec_probe(&self) {
        if self.codecs.load(std::sync::atomic::Ordering::Acquire) & STANDARD_CODECS == 0 {
            self.codec_probe_requested
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
    pub fn probe_codecs(self: &Arc<Self>) {
        let h = self.clone();
        std::thread::spawn(move || {
            use std::sync::atomic::Ordering;
            let mut retry = CodecRetry::new(Instant::now());
            let mut retrying = false;
            loop {
                if h.stop.load(Ordering::Acquire) {
                    return;
                }
                h.codec_probe_requested.store(false, Ordering::Release);
                h.probing_codecs.store(true, Ordering::Release);
                let mut config = h.config.read().unwrap().clone();
                // Probe the driver's ability independently of the stream's opt-in.
                config.values.insert("amd_ltr_frames".into(), "4".into());
                let result = h.probe_codecs_once(&config, retrying);
                retrying = true;
                let Some((flags, error, retry_pyrowave)) =
                    result.unwrap_or_else(|error| Some((0, Some(format!("{error:#}")), false)))
                else {
                    // A stream is starting: leave the encoder to it and probe
                    // again once it has ended, without judging this attempt.
                    h.probing_codecs.store(false, Ordering::Release);
                    h.metadata.lock().unwrap().take();
                    tracing::debug!("encoder capability probe paused for a stream");
                    if !h.wait_for_codec_retry(&retry) {
                        return;
                    }
                    continue;
                };
                h.codecs.store(flags, Ordering::Release);
                h.video_codecs_ready.send_replace(true);
                h.probing_codecs.store(false, Ordering::Release);
                if h.stop.load(Ordering::Acquire) {
                    return;
                }
                let now = Instant::now();
                let again = retry.completed(flags, error.as_deref(), &h.warnings, now);
                if flags & !0x40000000 != 0 {
                    tracing::info!(codec_flags = flags, "encoder capability probe completed");
                }
                h.metadata.lock().unwrap().take();
                if !again {
                    if retry_pyrowave {
                        h.retry_pyrowave_probe(&config);
                    }
                    return;
                }
                if !h.wait_for_codec_retry(&retry) {
                    return;
                }
            }
        });
    }
    /// One pass over every codec. A retry (`yield_to_streams`) stops before
    /// opening another encoder once a launch or stream appears, returning
    /// None: a probe must not compete with a stream for the encoder.
    fn probe_codecs_once(
        &self,
        config: &Config,
        yield_to_streams: bool,
    ) -> Result<Option<(u32, Option<String>, bool)>> {
        let _com = butterpollo_windows::capture::ComGuard::new()?;
        let busy = || yield_to_streams && !self.sessions.lock().unwrap().idle();
        // Until this pass publishes its result, keep advertising what the
        // separate PyroWave probe found last time.
        let pyrowave = self.codecs.load(std::sync::atomic::Ordering::Acquire) & PYROWAVE_CODECS;
        let mut first_error = None;
        let image = butterpollo_windows::capture::Image {
            width: 640,
            height: 480,
            stride: 2560,
            bytes: vec![128; 640 * 480 * 4],
            captured: Instant::now(),
            pixel: butterpollo_windows::capture::Pixel::Bgra8,
        };
        let mut flags = 0u32;
        let software = config.get("encoder", "auto") == "software";
        // Bits follow moonlight-common-c's SCM_* values; each 4:4:4 mode
        // is probed only after its 4:2:0 mode works.
        for (codec, hdr, yuv444, bit) in [
            (0, false, false, 1),
            (1, false, false, 0x100),
            (1, true, false, 0x200),
            (2, false, false, 0x10000),
            (2, true, false, 0x20000),
            (0, false, true, 0x40000),
            (1, false, true, 0x80000),
            (1, true, true, 0x100000),
            (2, false, true, 0x200000),
            (2, true, true, 0x400000),
        ] {
            let mode = config.integer(if codec == 1 { "hevc_mode" } else { "av1_mode" }, 0);
            if matches!(codec, 1 | 2) && (mode == 1 || (hdr && mode == 2)) {
                continue;
            }
            let base = match (codec, hdr) {
                (0, _) => 1,
                (1, false) => 0x100,
                (1, true) => 0x200,
                (2, false) => 0x10000,
                _ => 0x20000,
            };
            if yuv444 && flags & base == 0 {
                continue;
            }
            if self.stop.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            if busy() {
                return Ok(None);
            }
            let negotiated = butterpollo_core::rtsp::Negotiated {
                width: 640,
                height: 480,
                fps: 30,
                bitrate_kbps: 2000,
                codec,
                hdr,
                yuv444,
                ..Default::default()
            };
            match butterpollo_windows::encoder::Encoder::new_options(
                &negotiated,
                config.get("encoder", "auto"),
                config.get("output_name", ""),
                config,
            ) {
                // A 4:4:4 stream must not quietly fall back to software
                // encoding; advertise it only from a hardware encoder.
                Ok(encoder) if yuv444 && !software && !encoder.hardware() => {
                    tracing::debug!(codec, hdr, "4:4:4 needs a hardware encoder; not advertised");
                }
                Ok(mut encoder) => {
                    if codec == 0 && !yuv444 {
                        *self.probed_encoder.lock().unwrap() = encoder.backend();
                    }
                    for frame in 0..8 {
                        match encoder.encode(&image, frame == 0, negotiated.bitrate_kbps) {
                            Ok(packets) if !packets.is_empty() => {
                                flags |= bit;
                                self.codecs
                                    .store(flags | pyrowave, std::sync::atomic::Ordering::Release);
                                if encoder.supports_invalidation() {
                                    flags |= 0x40000000;
                                }
                                break;
                            }
                            Err(error) => {
                                first_error.get_or_insert_with(|| format!("{error:#}"));
                                tracing::debug!(%error, codec, hdr, "encoder capability probe failed");
                                break;
                            }
                            _ => std::thread::sleep(Duration::from_millis(5)),
                        }
                    }
                }
                // Most GPUs cannot encode 4:4:4; that is not a fault.
                Err(error) if yuv444 => {
                    tracing::debug!(%error, codec, hdr, "4:4:4 encoding unavailable")
                }
                Err(error) => {
                    first_error.get_or_insert_with(|| format!("{error:#}"));
                    tracing::debug!(%error, codec, hdr, "encoder capability initialization failed");
                }
            }
        }
        // Publish standard codecs together before the optional Vulkan
        // probe. A driver/overlay failure in that child cannot kill them.
        self.codecs
            .store(flags | pyrowave, std::sync::atomic::Ordering::Release);
        self.video_codecs_ready.send_replace(true);
        let mut retry_pyrowave = false;
        if busy() {
            return Ok(None);
        }
        if config.boolean("pyrowave", true) && !self.stop.load(std::sync::atomic::Ordering::Acquire)
        {
            match butterpollo_windows::codec_probe::pyrowave(config) {
                Ok(optional) => flags |= optional,
                Err(error) => {
                    retry_pyrowave =
                        error.is::<butterpollo_windows::codec_probe::SessionNotReady>();
                    if flags & STANDARD_CODECS != 0 {
                        tracing::warn!(error = %format!("{error:#}"), retry = retry_pyrowave, "optional PyroWave probe failed; standard codecs remain available");
                    } else {
                        tracing::debug!(error = %format!("{error:#}"), "optional PyroWave probe failed");
                    }
                }
            }
        }
        Ok(Some((flags, first_error, retry_pyrowave)))
    }
    fn wait_for_codec_retry(&self, retry: &CodecRetry) -> bool {
        use std::sync::atomic::Ordering;
        let mut session_check = Instant::now();
        loop {
            if self.stop.load(Ordering::Acquire) {
                return false;
            }
            let now = Instant::now();
            if now >= session_check
                && retry.due(now, self.codec_probe_requested.load(Ordering::Acquire))
            {
                if self.probe_session_ready() {
                    return true;
                }
                session_check = now + CODEC_RETRY_MIN;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    /// A user is signed in, and no stream runs or is being launched: a
    /// launch asks for a probe, which must wait until its stream has ended.
    fn probe_session_ready(&self) -> bool {
        !(butterpollo_windows::process::is_system()
            && !butterpollo_windows::process::user_signed_in())
            && self.sessions.lock().unwrap().idle()
    }
    /// Wait `delay`, then until a user is signed in and no stream runs or starts.
    /// False when the host is stopping.
    fn wait_for_session(&self, delay: Duration) -> bool {
        use std::sync::atomic::Ordering;
        let mut until = Instant::now() + delay;
        loop {
            while Instant::now() < until {
                if self.stop.load(Ordering::Acquire) {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            // Wait for a sign-in, and leave the GPU to a running stream.
            if !self.probe_session_ready() {
                until = Instant::now() + Duration::from_secs(5);
                continue;
            }
            return true;
        }
    }
    /// Probe PyroWave again when the first probe ran before the signed-in
    /// user's session was ready, as when the host starts with Windows.
    /// PyroWave otherwise stayed unavailable until the host restarted.
    fn retry_pyrowave_probe(&self, config: &Config) {
        use butterpollo_windows::codec_probe;
        use std::sync::atomic::Ordering;
        let mut delay = Duration::from_secs(5);
        for attempts in 1..=6 {
            if !self.wait_for_session(delay) {
                return;
            }
            match codec_probe::pyrowave(config) {
                Ok(optional) => {
                    let flags = self.codecs.fetch_or(optional, Ordering::AcqRel) | optional;
                    self.metadata.lock().unwrap().take();
                    tracing::info!(
                        codec_flags = flags,
                        attempts,
                        "optional PyroWave probe completed once the session was ready"
                    );
                    return;
                }
                Err(error) if error.is::<codec_probe::SessionNotReady>() && attempts < 6 => {
                    tracing::debug!(error = %format!("{error:#}"), attempts, "optional PyroWave probe is still waiting for the session");
                }
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), attempts, "optional PyroWave probe failed again; standard codecs remain available");
                    return;
                }
            }
            delay = (delay * 2).min(Duration::from_secs(60));
        }
    }
    pub fn save_credentials(&self, credentials: &Credentials) -> Result<()> {
        let mut paired = self.paired.write().unwrap();
        let mut document = butterpollo_core::state::load_json(&self.credentials_path, json!({}))?;
        let values = serde_json::to_value(credentials)?;
        for (key, value) in values.as_object().unwrap() {
            document[key] = value.clone();
        }
        butterpollo_core::state::write_json(&self.credentials_path, &document)?;
        if self.credentials_path == self.paired_path {
            paired.document = document;
        }
        Ok(())
    }
    pub fn save_web_sessions(&self, sessions: &HashMap<String, WebSession>) -> Result<()> {
        let mut aliases = self.aliases.lock().unwrap();
        let mut document = aliases.clone();
        if !document["root"].is_object() {
            document["root"] = json!({});
        }
        document["root"]["session_tokens"] = Value::Array(
            sessions
                .iter()
                .filter(|(_, s)| s.refresh_expires > Instant::now())
                .map(|(key, s)| web_sessions::record(key, s))
                .collect(),
        );
        butterpollo_core::state::write_json(&self.aliases_path, &document)?;
        *aliases = document;
        Ok(())
    }
    pub fn new_web_session(
        &self,
        username: String,
        remember_me: bool,
        user_agent: String,
        remote_address: String,
        previous: Option<WebSession>,
    ) -> Result<(String, String, String, u64)> {
        let ttl = self
            .config
            .read()
            .unwrap()
            .integer("session_token_ttl_seconds", 7200)
            .clamp(60, 604800) as u64;
        let access = hex::encode(butterpollo_core::crypto::random::<32>());
        let refresh = hex::encode(butterpollo_core::crypto::random::<32>());
        let csrf = hex::encode(butterpollo_core::crypto::random::<32>());
        let wall = web_sessions::now();
        let refresh_deadline = previous
            .as_ref()
            .map(|s| s.refresh_deadline)
            .unwrap_or_else(|| {
                wall + if remember_me {
                    self.config
                        .read()
                        .unwrap()
                        .integer("remember_me_refresh_token_ttl_seconds", 604800)
                        .clamp(60, 366 * 86400) as u64
                } else {
                    ttl.max(86400)
                }
            });
        let access_deadline = (wall + ttl).min(refresh_deadline);
        let mut extra = previous
            .as_ref()
            .map(|s| s.extra.clone())
            .unwrap_or_default();
        extra.insert(
            "rotation_id".into(),
            hex::encode(butterpollo_core::crypto::random::<12>()).into(),
        );
        let mut sessions = self.web_sessions.lock().unwrap();
        if let Some(previous) = previous.as_ref()
            && !sessions
                .values()
                .any(|s| s.refresh == previous.refresh && s.refresh_expires > Instant::now())
        {
            return Err(web_sessions::Rotated.into());
        }
        let mut next = sessions.clone();
        next.retain(|_, s| s.refresh_expires > Instant::now());
        if let Some(previous) = previous.as_ref() {
            next.retain(|_, s| s.refresh != previous.refresh);
        }
        if next.len() >= 64
            && let Some(key) = next
                .iter()
                .min_by_key(|(_, s)| s.created)
                .map(|(key, _)| key.clone())
        {
            next.remove(&key);
        }
        next.insert(
            web_sessions::hash(&access),
            WebSession {
                created: wall,
                last_seen: wall,
                refresh: web_sessions::hash(&refresh),
                csrf: csrf.clone(),
                expires: Instant::now() + Duration::from_secs(access_deadline.saturating_sub(wall)),
                refresh_expires: Instant::now()
                    + Duration::from_secs(refresh_deadline.saturating_sub(wall)),
                access_deadline,
                refresh_deadline,
                username,
                remember_me,
                user_agent,
                remote_address,
                extra,
            },
        );
        self.save_web_sessions(&next)?;
        *sessions = next;
        Ok((access, refresh, csrf, refresh_deadline.saturating_sub(wall)))
    }
}
/// The app library. One that cannot be read as a library starts the host
/// with no apps, as in Vibepollo, rather than keeping the host and its web
/// console down; the file is copied beside it so a later save cannot lose it.
fn load_library(path: &std::path::Path, default: Value) -> Result<Value> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(default),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let Some(json) = butterpollo_core::state::json_bytes(&bytes) else {
        return Ok(default);
    };
    let parsed = serde_json::from_slice::<Value>(json)
        .map_err(anyhow::Error::from)
        .and_then(|document| {
            serde_json::from_value::<Vec<App>>(document.get("apps").cloned().unwrap_or(json!([])))?;
            Ok(document)
        });
    match parsed {
        Ok(document) => Ok(document),
        Err(error) => {
            let mut kept = path.as_os_str().to_owned();
            kept.push(".invalid");
            let kept = PathBuf::from(kept);
            butterpollo_core::state::atomic_write(&kept, &bytes)?;
            tracing::error!(
                error = %format!("{error:#}"),
                file = %path.display(),
                kept = %kept.display(),
                "the app library is not valid; starting with no apps"
            );
            Ok(json!({"env":{},"apps":[]}))
        }
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn a_rejoin_in_the_same_mode_reuses_the_clients_own_display_untouched() {
        use super::take_retained;
        use std::collections::BTreeMap;
        use std::sync::Arc;
        use std::time::Instant;
        let paused = Some(Instant::now());
        let mine = Arc::new((2560, 1440, 120_000, true));
        let theirs = Arc::new((2560, 1440, 120_000, true));
        let mut displays = BTreeMap::from([
            ("me".to_string(), (mine.clone(), paused)),
            ("other".to_string(), (theirs.clone(), None)),
        ]);
        let same = |lease: &Arc<(u32, u32, u32, bool)>| **lease == (2560, 1440, 120_000, true);

        // The same client in the same mode gets the very display it had, and
        // the retained entry stays as it was: nothing is created or released.
        let reused = take_retained(&mut displays, "me", same).unwrap();
        assert!(Arc::ptr_eq(&reused, &mine));
        assert_eq!(displays.len(), 2);
        assert!(Arc::ptr_eq(&displays["me"].0, &mine));
        assert_eq!(displays["me"].1, paused);
        drop(reused);
        // Rejoining again still finds it.
        assert!(Arc::ptr_eq(
            &take_retained(&mut displays, "me", same).unwrap(),
            &mine
        ));

        // A client with nothing retained never takes another client's display.
        assert_eq!(take_retained(&mut displays, "new", same), Err(None));
        assert!(Arc::ptr_eq(&displays["other"].0, &theirs));
        assert!(Arc::ptr_eq(&displays["me"].0, &mine));
    }
    #[test]
    fn a_rejoin_in_another_mode_releases_only_the_clients_own_display() {
        use super::take_retained;
        use std::collections::BTreeMap;
        use std::sync::Arc;
        let mine = Arc::new((2560, 1440, 120_000, true));
        let theirs = Arc::new((2560, 1440, 120_000, true));
        let mut displays = BTreeMap::from([
            ("me".to_string(), (mine.clone(), None)),
            ("other".to_string(), (theirs.clone(), None)),
        ]);
        let sdr = |lease: &Arc<(u32, u32, u32, bool)>| **lease == (2560, 1440, 120_000, false);
        let released = take_retained(&mut displays, "me", sdr)
            .unwrap_err()
            .unwrap();
        assert!(Arc::ptr_eq(&released, &mine));
        assert!(!displays.contains_key("me"));
        assert!(Arc::ptr_eq(&displays["other"].0, &theirs));
        assert_eq!(take_retained(&mut displays, "me", sdr), Err(None));
    }
    #[test]
    fn host_load_reuses_the_shared_virtual_display_guid_without_replacing_an_existing_one() {
        use super::test_support::Fixture;
        let f = Fixture::new();
        let imported = "f773d31b-43da-470c-80d5-02e777a6d993";
        let existing = "15ff84bd-9db8-45ef-bfde-e60337744358";
        butterpollo_core::state::write_json(
            &f.host.aliases_path,
            &serde_json::json!({"root":{"shared_virtual_display_guid":imported}}),
        )
        .unwrap();
        let load =
            || super::Host::load(f.host.directory.clone(), f.host.assets.clone(), None).unwrap();
        let host = load();
        assert_eq!(
            host.paired.read().unwrap().document["root"]["shared_virtual_display_guid"],
            imported
        );
        let mut state = host.paired.write().unwrap();
        state.document["root"]["shared_virtual_display_guid"] = existing.into();
        state.save(&host.paired_path).unwrap();
        drop(state);
        assert_eq!(
            load().paired.read().unwrap().document["root"]["shared_virtual_display_guid"],
            existing
        );
    }
    use super::*;
    #[test]
    fn codecs_keep_retrying_with_capped_backoff_until_the_probe_recovers() {
        let mut now = Instant::now();
        let mut retry = CodecRetry::new(now);
        let warnings = Warnings::default();
        let mut attempts = 0;
        let mut probe = || {
            attempts += 1;
            if attempts <= 100 {
                (0, Some("AMF error 1"))
            } else {
                (0x101, None)
            }
        };
        for delay in [5, 10, 20, 40]
            .into_iter()
            .chain(std::iter::repeat_n(60, 96))
        {
            let (flags, error) = probe();
            assert!(retry.completed(flags, error, &warnings, now));
            assert_eq!(
                warnings.snapshot()[0].message,
                "No video encoder available: AMF error 1. Retrying."
            );
            now += Duration::from_secs(delay);
            assert!(!retry.due(now - Duration::from_millis(1), false));
            assert!(retry.due(now, false));
        }
        let (flags, error) = probe();
        assert!(!retry.completed(flags, error, &warnings, now));
        assert!(warnings.snapshot().is_empty());
        assert!(!retry.due(now + Duration::from_secs(600), true));
        assert_eq!(attempts, 101);
    }
    #[test]
    fn client_requests_bypass_backoff_but_never_the_minimum_interval() {
        let mut now = Instant::now();
        let mut retry = CodecRetry::new(now);
        let warnings = Warnings::default();
        let mut calls = 0;
        let mut probe = || {
            calls += 1;
            0
        };
        for _ in 0..10 {
            assert!(retry.completed(probe(), Some("driver unavailable"), &warnings, now));
            for milliseconds in [0, 1, 250, 4999] {
                assert!(!retry.due(now + Duration::from_millis(milliseconds), true));
            }
            now += CODEC_RETRY_MIN;
            assert!(retry.due(now, true));
        }
        assert_eq!(calls, 10);
        assert!(!retry.due(now, false));
        assert!(retry.due(now + Duration::from_secs(55), false));
    }
    #[test]
    fn codec_failure_reminders_are_limited_and_recovery_resets_the_delay() {
        let now = Instant::now();
        let mut retry = CodecRetry::new(now);
        assert!(retry.warn(now));
        for seconds in 1..60 {
            assert!(!retry.warn(now + Duration::from_secs(seconds)));
        }
        assert!(retry.warn(now + Duration::from_secs(60)));
        let warnings = Warnings::default();
        for _ in 0..4 {
            assert!(retry.completed(0, None, &warnings, now));
        }
        assert!(!retry.due(now + Duration::from_secs(39), false));
        assert!(!retry.completed(1, None, &warnings, now));
        assert!(retry.completed(0, Some("COM initialization failed"), &warnings, now));
        assert_eq!(
            warnings.snapshot()[0].message,
            "No video encoder available: COM initialization failed. Retrying."
        );
        assert!(!retry.due(now + Duration::from_secs(4), false));
        assert!(retry.due(now + Duration::from_secs(5), false));
    }
    #[test]
    fn codec_modifiers_do_not_count_as_video_and_optional_codecs_keep_standard_retries() {
        let now = Instant::now();
        let mut retry = CodecRetry::new(now);
        let warnings = Warnings::default();
        assert!(retry.completed(0x40000000, None, &warnings, now));
        assert_eq!(
            warnings.snapshot()[0].message,
            "No video encoder available: the encoder produced no packets. Retrying."
        );
        assert!(retry.completed(0x800000, None, &warnings, now));
        assert!(warnings.snapshot().is_empty());
        assert!(retry.completed(0, Some("AMF error 1"), &warnings, now));
        assert_eq!(warnings.snapshot().len(), 1);
        assert!(!retry.completed(0x10000, None, &warnings, now));
        assert!(warnings.snapshot().is_empty());
    }
    #[test]
    fn an_invalid_library_starts_empty_and_is_kept() {
        let dir = std::env::temp_dir().join(format!("butterpollo-apps-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("apps.json");
        let default = json!({"apps":[{"name":"Desktop"}]});
        assert_eq!(load_library(&path, default.clone()).unwrap(), default);
        std::fs::write(&path, b"{\"apps\":[{\"name\":\"Game\",}]}").unwrap();
        assert_eq!(
            load_library(&path, default.clone()).unwrap()["apps"],
            json!([])
        );
        assert_eq!(
            std::fs::read(dir.join("apps.json.invalid")).unwrap(),
            b"{\"apps\":[{\"name\":\"Game\",}]}"
        );
        std::fs::write(&path, b"{\"apps\":[{\"name\":\"Game\"}]}").unwrap();
        assert_eq!(
            load_library(&path, default.clone()).unwrap()["apps"][0]["name"],
            "Game"
        );
        // Notepad's byte order mark, and a blank file Vibepollo reads as missing.
        std::fs::write(&path, b"\xef\xbb\xbf{\"apps\":[{\"name\":\"Noted\"}]}").unwrap();
        assert_eq!(
            load_library(&path, default.clone()).unwrap()["apps"][0]["name"],
            "Noted"
        );
        std::fs::write(&path, b"\r\n").unwrap();
        assert_eq!(load_library(&path, default.clone()).unwrap(), default);
        std::fs::remove_dir_all(&dir).ok();
    }
}
