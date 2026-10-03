use anyhow::{Context, Result};
use butterpollo_core::{
    config::Config,
    crypto::Identity,
    pairing::Pairings,
    session::Sessions,
    state::{App, Credentials, PairedState},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};
pub type Shared = Arc<Host>;
use crate::web_sessions::{self, WebSession};
pub struct PendingPin {
    pub name: String,
    pub created: Instant,
    pub sender: tokio::sync::oneshot::Sender<(String, String)>,
}
/// A client's display lease and when it last stopped being streamed.
pub type RetainedDisplay = (Arc<crate::display_session::Ready>, Option<Instant>);
pub struct Host {
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
    pub web_sessions: Mutex<HashMap<String, WebSession>>,
    pub stop: std::sync::atomic::AtomicBool,
    pub restart: std::sync::atomic::AtomicBool,
    pub codecs: std::sync::atomic::AtomicU32,
    pub probing_codecs: std::sync::atomic::AtomicBool,
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
    pub assets: PathBuf,
}
impl Host {
    pub fn stop_app(&self) {
        self.current_app.lock().unwrap().take();
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
        let paired_path = config.path("file_state", &directory, "sunshine_state.json");
        let credential_default = if directory.join("sunshine_credentials.json").exists() {
            "sunshine_credentials.json"
        } else {
            "sunshine_state.json"
        };
        let credentials_path = config.path("credentials_file", &directory, credential_default);
        let apps_path = config.path("file_apps", &directory, "apps.json");
        let aliases_path = config.path("vibeshine_file_state", &directory, "vibeshine_state.json");
        let mut aliases = butterpollo_core::state::load_json(&aliases_path, json!({"root":{}}))?;
        let paired = PairedState::load(&paired_path)?;
        paired.save(&paired_path)?;
        let certificate = config.path("cert", &directory, "credentials/cacert.pem");
        let key = config.path("pkey", &directory, "credentials/cakey.pem");
        let identity = Identity::load(&certificate, &key)?;
        let credential_doc = butterpollo_core::state::load_json(&credentials_path, json!({}))?;
        let credentials: Option<Credentials> = if credential_doc.get("username").is_some() {
            Some(serde_json::from_value(credential_doc).context("invalid web credentials")?)
        } else {
            None
        };
        let app_document = butterpollo_core::state::load_json(
            &apps_path,
            json!({"env":{},"apps":[App::desktop()]}),
        )?;
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
            web_sessions: Mutex::new(web_sessions),
            stop: std::sync::atomic::AtomicBool::new(false),
            restart: std::sync::atomic::AtomicBool::new(false),
            codecs: std::sync::atomic::AtomicU32::new(0),
            probing_codecs: std::sync::atomic::AtomicBool::new(true),
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
        let mut ready = self.video_codecs_ready.subscribe();
        let _ = ready.wait_for(|ready| *ready).await;
    }
    pub fn probe_codecs(self: &Arc<Self>) {
        let h = self.clone();
        std::thread::spawn(move || {
            let Ok(_com) = butterpollo_windows::capture::ComGuard::new() else {
                h.video_codecs_ready.send_replace(true);
                h.probing_codecs
                    .store(false, std::sync::atomic::Ordering::Release);
                h.metadata.lock().unwrap().take();
                return;
            };
            let mut config = h.config.read().unwrap().clone();
            // Probe the driver's ability independently of the stream's opt-in.
            config.values.insert("amd_ltr_frames".into(), "4".into());
            let image = butterpollo_windows::capture::Image {
                width: 640,
                height: 480,
                stride: 2560,
                bytes: vec![128; 640 * 480 * 4],
                captured: Instant::now(),
                pixel: butterpollo_windows::capture::Pixel::Bgra8,
            };
            let mut flags = 0u32;
            for (codec, hdr, yuv444, bit) in [
                (0, false, false, 1),
                (1, false, false, 0x100),
                (1, true, false, 0x200),
                (2, false, false, 0x10000),
                (2, true, false, 0x20000),
                (3, false, false, 0x800000),
                (3, false, true, 0x1000000),
                (3, true, false, 0x2000000),
                (3, true, true, 0x4000000),
            ] {
                if codec == 3 {
                    // Publish standard codecs as a complete set before the
                    // optional PyroWave checks. A partial set can make clients
                    // permanently disable HDR in their cached app list.
                    h.video_codecs_ready.send_replace(true);
                }
                let mode = config.integer(if codec == 1 { "hevc_mode" } else { "av1_mode" }, 0);
                if matches!(codec, 1 | 2) && (mode == 1 || (hdr && mode == 2)) {
                    continue;
                }
                if codec == 3 && !config.boolean("pyrowave", true) {
                    continue;
                }
                if h.stop.load(std::sync::atomic::Ordering::Acquire) {
                    break;
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
                    &config,
                ) {
                    Ok(mut encoder) => {
                        for frame in 0..8 {
                            match encoder.encode(&image, frame == 0, negotiated.bitrate_kbps) {
                                Ok(packets) if !packets.is_empty() => {
                                    flags |= bit;
                                    h.codecs.store(flags, std::sync::atomic::Ordering::Release);
                                    if encoder.supports_invalidation() {
                                        flags |= 0x40000000;
                                    }
                                    break;
                                }
                                Err(error) => {
                                    tracing::warn!(%error, codec, hdr, "encoder capability probe failed");
                                    break;
                                }
                                _ => std::thread::sleep(Duration::from_millis(5)),
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, codec, hdr, "encoder capability initialization failed")
                    }
                }
            }
            h.codecs.store(flags, std::sync::atomic::Ordering::Release);
            h.video_codecs_ready.send_replace(true);
            h.probing_codecs
                .store(false, std::sync::atomic::Ordering::Release);
            h.metadata.lock().unwrap().take();
            tracing::info!(codec_flags = flags, "encoder capability probe completed");
        });
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
