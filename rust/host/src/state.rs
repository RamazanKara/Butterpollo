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
pub struct WebSession {
    pub created: u64,
    pub refresh: String,
    pub csrf: String,
    pub expires: Instant,
    pub refresh_expires: Instant,
    pub username: String,
}
pub struct PendingPin {
    pub name: String,
    pub created: Instant,
    pub sender: tokio::sync::oneshot::Sender<(String, String)>,
}
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
    pub current_app: Mutex<Option<crate::process::RunningApp>>,
    pub monitors: Mutex<BTreeMap<String, Arc<butterpollo_windows::display::Retained>>>,
    pub updates: Mutex<Value>,
    pub assets: PathBuf,
}
impl Host {
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
        let credentials = if credential_doc.get("username").is_some() {
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
            web_sessions: Mutex::new(HashMap::new()),
            stop: std::sync::atomic::AtomicBool::new(false),
            restart: std::sync::atomic::AtomicBool::new(false),
            codecs: std::sync::atomic::AtomicU32::new(1),
            current_app: Mutex::new(None),
            monitors: Mutex::new(BTreeMap::new()),
            updates: Mutex::new(
                json!({"status":true,"checking":false,"check_failed":false,"checked_at":0,"releases":[]}),
            ),
            assets,
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
    pub fn probe_codecs(self: &Arc<Self>) {
        let h = self.clone();
        std::thread::spawn(move || {
            let Ok(_com) = butterpollo_windows::capture::ComGuard::new() else {
                return;
            };
            let config = h.config.read().unwrap().clone();
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
            ] {
                if h.stop.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                let negotiated = butterpollo_core::rtsp::Negotiated {
                    width: 640,
                    height: 480,
                    codec,
                    hdr,
                    yuv444,
                    ..Default::default()
                };
                if let Ok(mut encoder) = butterpollo_windows::encoder::Encoder::new(
                    &negotiated,
                    config.get("encoder", "auto"),
                    config.get("output_name", ""),
                ) {
                    for frame in 0..8 {
                        match encoder.encode(&image, frame == 0, negotiated.bitrate_kbps) {
                            Ok(packets) if !packets.is_empty() => {
                                flags |= bit;
                                break;
                            }
                            Err(_) => break,
                            _ => std::thread::sleep(Duration::from_millis(5)),
                        }
                    }
                }
            }
            h.codecs.store(flags, std::sync::atomic::Ordering::Release);
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
    pub fn new_web_session(&self, username: String) -> (String, String, String) {
        let access = hex::encode(butterpollo_core::crypto::random::<32>());
        let refresh = hex::encode(butterpollo_core::crypto::random::<32>());
        let csrf = hex::encode(butterpollo_core::crypto::random::<32>());
        let mut sessions = self.web_sessions.lock().unwrap();
        sessions.retain(|_, s| s.refresh_expires > Instant::now());
        if sessions.len() >= 64
            && let Some(key) = sessions.keys().next().cloned()
        {
            sessions.remove(&key);
        }
        sessions.insert(
            access.clone(),
            WebSession {
                created: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                refresh: refresh.clone(),
                csrf: csrf.clone(),
                expires: Instant::now() + Duration::from_secs(3600),
                refresh_expires: Instant::now() + Duration::from_secs(86400),
                username,
            },
        );
        (access, refresh, csrf)
    }
}
