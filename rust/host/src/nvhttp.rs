use crate::{
    state::{PendingPin, Shared},
    tls::Connection,
};
use anyhow::{Context, Result, bail};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use butterpollo_core::{
    crypto,
    pairing::Pairing,
    session::{Launch, Role},
    state::Client,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
};
type Args = HashMap<String, String>;
#[derive(Debug)]
struct LaunchFailure(u16, &'static str);
impl std::fmt::Display for LaunchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.1)
    }
}
impl std::error::Error for LaunchFailure {}
fn remote_owner(h: &Shared, client: &str) -> butterpollo_core::remote::Owner {
    use butterpollo_core::remote::Owner;
    if h.monitors.lock().unwrap().contains_key(client) {
        return Owner::Monitor;
    }
    let sessions = h.sessions.lock().unwrap();
    if sessions
        .pending
        .values()
        .any(|s| s.client.uuid == client && s.role == Role::RemoteMonitor)
        || sessions.active.values().any(|s| {
            !s.stopping() && s.launch.client.uuid == client && s.launch.role == Role::RemoteMonitor
        })
    {
        Owner::Monitor
    } else if sessions
        .pending
        .values()
        .any(|s| s.client.uuid == client && s.role == Role::InputOnly)
        || sessions.active.values().any(|s| {
            !s.stopping() && s.launch.client.uuid == client && s.launch.role == Role::InputOnly
        })
    {
        Owner::Input
    } else {
        Owner::None
    }
}
fn remote_game(h: &Shared) -> Option<butterpollo_core::remote::Game> {
    let (id, name, owner, generation) = h.current_app.lock().unwrap().as_ref().map(|app| {
        (
            app.id,
            app.name.clone(),
            app.owner.clone(),
            app.generation.clone(),
        )
    })?;
    let uuid = h
        .apps
        .read()
        .unwrap()
        .iter()
        .find(|app| app.id() == id)
        .and_then(|app| app.extra.get("uuid"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    Some(butterpollo_core::remote::Game {
        app: butterpollo_core::remote::Entry {
            id,
            title: name,
            uuid,
            art_version: id.to_string(),
            ..Default::default()
        },
        owner,
        generation,
    })
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
pub fn xml(code: u16, fields: &[(&str, String)], error: Option<String>) -> Response {
    let mut s = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><root status_code=\"{code}\"");
    if let Some(error) = error {
        s.push_str(&format!(" status_message=\"{}\"", escape(&error)));
    }
    s.push('>');
    for (k, v) in fields {
        s.push_str(&format!("<{k}>{}</{k}>", escape(v)));
    }
    s.push_str("</root>");
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        s,
    )
        .into_response()
}
fn authenticated(h: &Shared, connection: &Connection, permission: u32) -> Result<Client> {
    if !connection.tls {
        bail!("TLS client authentication required");
    }
    let der = connection
        .certificate
        .as_deref()
        .context("client certificate required")?;
    let state = h.paired.read().unwrap();
    let c = state
        .client_by_certificate(der)
        .context("client is not paired or is disabled")?;
    if !c.allows(permission) {
        bail!("client permission denied");
    }
    Ok(c.clone())
}
fn stream_key_id(value: &str) -> Result<u32> {
    // Android sends Java's signed random int; desktop clients can send the
    // same 32 bits as an unsigned decimal. Both forms describe the same IV.
    value
        .parse::<u32>()
        .or_else(|_| value.parse::<i32>().map(|id| id as u32))
        .context("stream key ID must be a signed or unsigned 32-bit decimal integer")
}
pub fn router(h: Shared, https: bool) -> Router {
    let r = Router::new()
        .route("/serverinfo", get(serverinfo))
        .route("/pair", get(pair).post(pair))
        .route("/pair/", get(pair).post(pair));
    let r = if https {
        r.route("/applist", get(applist))
            .route("/launch", get(launch))
            .route("/resume", get(resume))
            .route("/cancel", get(cancel))
            .route("/unpair", get(unpair).post(unpair))
            .route("/appasset", get(appasset))
            .route("/bitrate", get(bitrate))
            .route("/api/abr/capabilities", get(abr))
            .route("/pyrowave-bandwidth-probe", get(pyrowave_bandwidth))
            .route(
                "/actions/clipboard",
                get(clipboard_read).post(clipboard_write),
            )
    } else {
        r
    };
    r.layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
        .with_state(h)
}
async fn serverinfo(
    State(h): State<Shared>,
    Extension(connection): Extension<Connection>,
) -> Response {
    h.wait_for_video_codecs().await;
    let config = h.config.read().unwrap().clone();
    let ports = config.ports().unwrap();
    let client = authenticated(&h, &connection, 0).ok();
    let paired = client.is_some();
    let pyrowave_link = if paired {
        butterpollo_windows::net::routed_link_bps(connection.peer) / 1_000_000
    } else {
        0
    };
    let address = connection.local.ip();
    // Only paired clients learn the MAC address (for wake-on-LAN).
    let mac = if paired {
        tokio::task::spawn_blocking(move || butterpollo_windows::net::local_mac(address))
            .await
            .ok()
            .and_then(Result::ok)
    } else {
        None
    }
    .unwrap_or_else(|| "00:00:00:00:00:00".into());
    let windows_11 = butterpollo_windows::display::windows_11();
    let (limiter, virtual_limiter, limit) = butterpollo_core::framegen::advertised(
        &config,
        config.virtual_display_mode(windows_11) != "disabled",
    );
    let driver_ready =
        tokio::task::spawn_blocking(butterpollo_windows::display::virtual_display_available)
            .await
            .unwrap_or(false);
    let permission = client.as_ref().map_or(0, |client| client.perm);
    // Artemis lists these commands; a client runs one by its index.
    let commands: Vec<String> = if permission & 0x0010_0000 != 0 {
        serde_json::from_str::<serde_json::Value>(config.get("server_cmd", "[]"))
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .map(|command| {
                command
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    } else {
        Vec::new()
    };
    // Moonlight stores LocalIP as the IPv4 LAN address; over IPv6 it expects
    // GFE's placeholder, as Vibepollo sends.
    let local_ip = match connection.local.ip().to_canonical() {
        std::net::IpAddr::V6(_) => "127.0.0.1".to_owned(),
        v4 => v4.to_string(),
    };
    let game = remote_game(&h);
    // GameStream's public state is scoped to the requesting client. Local
    // maintenance needs the actual session counts, including queued launches.
    let local = connection.peer.ip().to_canonical().is_loopback();
    let (session_count, pending_count) = if local {
        let mut sessions = h.sessions.lock().unwrap();
        sessions.expire();
        (
            sessions.active.len().to_string(),
            sessions.pending.len().to_string(),
        )
    } else {
        (String::new(), String::new())
    };
    let current = game
        .as_ref()
        .filter(|game| {
            client.as_ref().is_some_and(|client| {
                (game.owner == client.uuid
                    && remote_owner(&h, &client.uuid) == butterpollo_core::remote::Owner::None)
                    || h.confirmations.lock().unwrap().active(
                        &client.uuid,
                        butterpollo_core::remote::Confirmation::Replace,
                        &game.generation,
                    )
            })
        })
        .map_or(0, |game| game.app.id);
    let current_uuid = game
        .as_ref()
        .filter(|game| game.app.id == current && current != 0)
        .map(|game| game.app.uuid.clone())
        .unwrap_or_default();
    let mut fields = vec![
        ("hostname", crate::network::host_name(&config)),
        ("appversion", "7.1.431.-1".into()),
        ("GfeVersion", "3.23.0.74".into()),
        ("uniqueid", h.paired.read().unwrap().unique_id.clone()),
        ("HttpsPort", ports.https.to_string()),
        ("ExternalPort", ports.http.to_string()),
        ("PairStatus", u8::from(paired).to_string()),
        ("currentgame", current.to_string()),
        ("currentgameuuid", current_uuid),
        (
            "state",
            if current == 0 {
                "SUNSHINE_SERVER_FREE"
            } else {
                "SUNSHINE_SERVER_BUSY"
            }
            .into(),
        ),
        ("LocalIP", local_ip),
        ("mac", mac),
        (
            "MaxLumaPixelsHEVC",
            if h.codecs.load(std::sync::atomic::Ordering::Acquire) & 0x100 != 0 {
                "1869449984"
            } else {
                "0"
            }
            .into(),
        ),
        (
            "ServerCodecModeSupport",
            (h.codecs.load(std::sync::atomic::Ordering::Acquire) & !0x40000000).to_string(),
        ),
        ("RustHostVersion", env!("CARGO_PKG_VERSION").into()),
        ("RustHostSessionCount", session_count),
        ("RustHostPendingSessionCount", pending_count),
        (
            "RustHostApplicationActive",
            if local {
                u8::from(game.is_some()).to_string()
            } else {
                String::new()
            },
        ),
        (
            "RustHostProfile",
            if connection.peer.ip().to_canonical().is_loopback() {
                butterpollo_core::migration::profile_id(&h.directory)
            } else {
                String::new()
            },
        ),
        ("PyroWaveHostLinkMbps", pyrowave_link.to_string()),
        (
            "PyroWaveBandwidthProbeBytes",
            if paired { "33554432" } else { "0" }.into(),
        ),
        ("Permission", permission.to_string()),
        ("FrameLimiterSupported", "1".into()),
        ("FrameLimiterEnabled", u8::from(limiter).to_string()),
        (
            "VirtualDisplayFrameLimiterEnabled",
            u8::from(virtual_limiter).to_string(),
        ),
        ("FrameLimiterFpsLimitMilliHz", limit.to_string()),
        // Artemis offers virtual-display launches only when capable.
        ("VirtualDisplayCapable", "true".into()),
        ("VirtualDisplayDriverReady", driver_ready.to_string()),
        ("VirtualDisplayHDRCapable", "true".into()),
    ];
    fields.extend(commands.into_iter().map(|name| ("ServerCommand", name)));
    xml(200, &fields, None)
}
async fn pyrowave_bandwidth(
    State(h): State<Shared>,
    Extension(connection): Extension<Connection>,
) -> Response {
    if authenticated(&h, &connection, 0).is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    static PAYLOAD: std::sync::OnceLock<Bytes> = std::sync::OnceLock::new();
    let payload = PAYLOAD
        .get_or_init(|| Bytes::from(vec![0xa5; 32 * 1024 * 1024]))
        .clone();
    (
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        payload,
    )
        .into_response()
}
async fn pair(State(h): State<Shared>, Query(args): Query<Args>) -> Response {
    match do_pair(h, &args).await {
        Ok(fields) => xml(
            200,
            &fields
                .iter()
                .map(|(k, v)| (k.as_str(), v.clone()))
                .collect::<Vec<_>>(),
            None,
        ),
        Err(e) => xml(400, &[("paired", "0".into())], Some(e.to_string())),
    }
}
async fn do_pair(h: Shared, args: &Args) -> Result<Vec<(String, String)>> {
    let id = args.get("uniqueid").context("missing uniqueid")?.clone();
    if id.is_empty()
        || id.len() > 256
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        bail!("invalid uniqueid");
    }
    if !h.config.read().unwrap().boolean("enable_pairing", true) {
        bail!("pairing is disabled");
    }
    if args.get("phrase").is_some_and(|s| s == "getservercert") {
        let text = args
            .get("clientcert")
            .context("missing client certificate")?;
        if text.len() > 32768 {
            bail!("certificate too large");
        }
        let certificate = String::from_utf8(hex::decode(text)?)?;
        crypto::public_key(&certificate)?;
        let salt = hex::decode(args.get("salt").context("missing salt")?)?;
        if salt.len() < 16 || salt.len() > 32 {
            bail!("invalid salt length");
        }
        let name = args
            .get("devicename")
            .cloned()
            .unwrap_or("Moonlight Client".into());
        if let Some(auth) = args.get("otpauth") {
            // One-time PIN pairing (Artemis). A wrong hash still gets an
            // ordinary answer, with a random PIN that fails the next step.
            let otp = h
                .otp
                .lock()
                .unwrap()
                .take()
                .filter(|otp| otp.created.elapsed() < Duration::from_secs(180))
                .context("OTP pairing is not available")?;
            let salt_text = args.get("salt").map_or("", String::as_str);
            let expected = hex::encode_upper(crypto::hash(
                format!("{}{salt_text}{}", otp.pin, otp.passphrase).as_bytes(),
            ));
            let (pin, name) = if expected.eq_ignore_ascii_case(auth) {
                tracing::info!(client=%name, "Pairing with a one-time PIN");
                let name = if otp.device_name.is_empty() {
                    name
                } else {
                    otp.device_name
                };
                (otp.pin, name)
            } else {
                tracing::warn!(client=%name, "One-time PIN pairing failed");
                *h.otp.lock().unwrap() = Some(otp);
                (
                    format!(
                        "{:04}",
                        rand::Rng::gen_range(&mut rand::thread_rng(), 0..10_000u16)
                    ),
                    name,
                )
            };
            let pair = Pairing::new(id, name, certificate, &salt, &pin)?;
            h.pairings.lock().unwrap().insert(pair)?;
            return Ok(vec![
                ("paired".into(), "1".into()),
                (
                    "plaincert".into(),
                    hex::encode(h.identity.certificate.as_bytes()),
                ),
            ]);
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        {
            let mut pins = h.pins.lock().unwrap();
            pins.retain(|_, p| p.created.elapsed() < Duration::from_secs(300));
            if pins.contains_key(&id)
                || pins.len() >= 32
                || h.pairings.lock().unwrap().sessions.contains_key(&id)
            {
                bail!("pairing already pending or request limit reached");
            }
            pins.insert(
                id.clone(),
                PendingPin {
                    name: name.clone(),
                    created: Instant::now(),
                    sender,
                },
            );
        }
        tracing::info!(client=%name,"Pairing PIN required in the web interface");
        let response = tokio::time::timeout(Duration::from_secs(300), receiver).await;
        h.pins.lock().unwrap().remove(&id);
        let (pin, name) = response
            .context("PIN entry timed out")?
            .context("pairing cancelled")?;
        let pair = Pairing::new(id, name, certificate, &salt, &pin)?;
        h.pairings.lock().unwrap().insert(pair)?;
        return Ok(vec![
            ("paired".into(), "1".into()),
            (
                "plaincert".into(),
                hex::encode(h.identity.certificate.as_bytes()),
            ),
        ]);
    }
    if args.get("phrase").is_some_and(|s| s == "pairchallenge") {
        return Ok(vec![("paired".into(), "1".into())]);
    }
    let mut pairings = h.pairings.lock().unwrap();
    pairings.expire();
    let p = pairings
        .sessions
        .get_mut(&id)
        .context("no pending pairing")?;
    let result = (|| -> Result<Vec<(String, String)>> {
        if let Some(v) = args.get("clientchallenge") {
            if v.len() != 32 {
                bail!("invalid challenge length");
            }
            let response = p.client_challenge(&h.identity, &hex::decode(v)?)?;
            Ok(vec![
                ("paired".into(), "1".into()),
                ("challengeresponse".into(), hex::encode(response)),
            ])
        } else if let Some(v) = args.get("serverchallengeresp") {
            if v.len() != 64 {
                bail!("invalid challenge response length");
            }
            let response = p.server_response(&h.identity, &hex::decode(v)?)?;
            Ok(vec![
                ("paired".into(), "1".into()),
                ("pairingsecret".into(), hex::encode(response)),
            ])
        } else if let Some(v) = args.get("clientpairingsecret") {
            if v.len() != 544 {
                bail!("invalid pairing secret length");
            }
            p.finish(&hex::decode(v)?)?;
            let mut state = h.paired.write().unwrap();
            let perm = if state.clients.is_empty() {
                0x071f1f00
            } else {
                0x03000000
            };
            state.add(
                &h.paired_path,
                Client {
                    name: p.name.clone(),
                    cert: p.certificate.clone(),
                    uuid: uuid::Uuid::new_v4().to_string(),
                    perm,
                    enabled: true,
                    extra: BTreeMap::new(),
                },
            )?;
            tracing::info!(client=%p.name,"Client paired and saved");
            Ok(vec![("paired".into(), "1".into())])
        } else {
            bail!("unknown pairing phase")
        }
    })();
    if result.is_err() || args.contains_key("clientpairingsecret") {
        pairings.sessions.remove(&id);
    }
    result
}
async fn applist(
    State(h): State<Shared>,
    Extension(connection): Extension<Connection>,
) -> Response {
    let client = match authenticated(&h, &connection, 1 << 24) {
        Ok(c) => c,
        Err(e) => return xml(401, &[], Some(e.to_string())),
    };
    h.wait_for_video_codecs().await;
    use butterpollo_core::remote::{self, Control};
    let configured = h
        .apps
        .read()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, app)| remote::Entry {
            id: app.id(),
            title: app.name.clone(),
            index,
            uuid: app
                .extra
                .get("uuid")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .into(),
            art_version: app.id().to_string(),
        })
        .collect();
    let config = h.config.read().unwrap().clone();
    let game = remote_game(&h);
    let projection = remote::project(
        &client.uuid,
        remote_owner(&h, &client.uuid),
        game.as_ref(),
        config.boolean("enable_input_only_mode", false),
        configured,
    );
    let entries: Vec<_> = projection
        .entries
        .into_iter()
        .filter(|entry| {
            let permission = match remote::identify(entry.id, &entry.uuid) {
                Some(Control::Resume | Control::RunningGame) => 1 << 25,
                Some(_) => 1 << 26,
                None => 1 << 24,
            };
            client.perm & permission != 0
        })
        .collect();
    let legacy = config.boolean("legacy_ordering", false)
        && client
            .extra
            .get("enable_legacy_ordering")
            .is_none_or(|value| value != false && value != "false");
    let hdr = u8::from(
        h.codecs.load(std::sync::atomic::Ordering::Acquire) & (0x200 | 0x20000 | 0x800000) != 0,
    );
    let mut s = "<?xml version=\"1.0\"?><root status_code=\"200\">".to_owned();
    for (index, entry) in entries.iter().enumerate() {
        let title = if legacy {
            remote::ordered_title(&entry.title, entries.len(), index)
        } else {
            entry.title.clone()
        };
        s.push_str(&format!("<App><AppTitle>{}</AppTitle><UUID>{}</UUID><IDX>{}</IDX><ID>{}</ID><ArtVersion>{}</ArtVersion><IsHdrSupported>{hdr}</IsHdrSupported></App>", escape(&title), escape(&entry.uuid), entry.index, entry.id, escape(&entry.art_version)));
    }
    s.push_str("</root>");
    ([(header::CONTENT_TYPE, "application/xml")], s).into_response()
}
async fn launch(
    State(h): State<Shared>,
    Extension(connection): Extension<Connection>,
    Query(args): Query<Args>,
) -> Response {
    start(h, connection, args, false)
}
async fn resume(
    State(h): State<Shared>,
    Extension(connection): Extension<Connection>,
    Query(args): Query<Args>,
) -> Response {
    start(h, connection, args, true)
}
fn start(h: Shared, connection: Connection, args: Args, resume: bool) -> Response {
    use butterpollo_core::remote::{self, Control};
    let requested = args
        .get("appid")
        .and_then(|id| id.parse::<u32>().ok())
        .unwrap_or(0);
    let active = remote_game(&h);
    let control = remote::identify(requested, args.get("appuuid").map_or("", String::as_str))
        .or_else(|| {
            active
                .as_ref()
                .filter(|game| requested == remote::running_game_id(game.app.id))
                .map(|_| Control::RunningGame)
        });
    let permission = match control {
        Some(Control::Terminate) => 1 << 26,
        Some(Control::Resume | Control::RunningGame) => 1 << 25,
        _ if resume => 1 << 25,
        _ => 1 << 26,
    };
    let client = match authenticated(&h, &connection, permission) {
        Ok(c) => c,
        Err(error) => {
            tracing::warn!(%error, app_id = requested, resume, "Moonlight launch authorization failed");
            return xml(401, &[], Some(error.to_string()));
        }
    };
    let _transition = h.launch_transition.lock().unwrap();
    match control {
        Some(Control::Terminate) => {
            let Some(game) = remote_game(&h) else {
                return xml(
                    409,
                    &[("gamesession", "0".into())],
                    Some("No application is running".into()),
                );
            };
            let guard = !h
                .config
                .read()
                .unwrap()
                .boolean("remote_monitor_terminate_on_first_request", false)
                && game.owner != client.uuid;
            let mut confirmations = h.confirmations.lock().unwrap();
            if guard
                && !confirmations.confirm(
                    &client.uuid,
                    remote::Confirmation::Terminate,
                    &game.generation,
                    game.app.id,
                    Instant::now(),
                )
            {
                return xml(410, &[("resume", "0".into()), ("gamesession", "0".into())], Some("This will close the active stream but leave Remote Monitor and Remote Input connected. Launch Terminate again within 60 seconds to confirm this was intentional.".into()));
            }
            confirmations.clear(&client.uuid, remote::Confirmation::Terminate);
            drop(confirmations);
            h.sessions.lock().unwrap().stop_role(Role::Stream, None);
            h.stop_app();
            return xml(
                410,
                &[("gamesession", "0".into())],
                Some("Application terminated".into()),
            );
        }
        Some(Control::DisconnectMonitor) => {
            crate::remote_display::disconnect(&h, Some(&client.uuid));
            return xml(
                410,
                &[("gamesession", "0".into())],
                Some("Remote monitor disconnected".into()),
            );
        }
        Some(Control::DisconnectInput) => {
            h.sessions
                .lock()
                .unwrap()
                .stop_role(Role::InputOnly, Some(&client.uuid));
            return xml(
                410,
                &[("gamesession", "0".into())],
                Some("Remote input disconnected".into()),
            );
        }
        _ => {}
    }
    let owner = remote_owner(&h, &client.uuid);
    if (control == Some(Control::Input) && owner != remote::Owner::None)
        || (control == Some(Control::Monitor) && owner == remote::Owner::Input)
    {
        return xml(
            409,
            &[("gamesession", "0".into())],
            Some("Remote session action conflicts with this client's current session state".into()),
        );
    }
    let resume = resume
        || matches!(control, Some(Control::Resume | Control::RunningGame))
        || (control == Some(Control::Monitor) && owner == remote::Owner::Monitor);
    let result = (|| -> Result<(String, String)> {
        let key: [u8; 16] = hex::decode(args.get("rikey").context("missing stream key")?)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid stream key length"))?;
        let key_id = stream_key_id(args.get("rikeyid").context("missing stream key ID")?)?;
        let mut role = match control {
            Some(Control::Monitor) => Role::RemoteMonitor,
            Some(Control::Input) => Role::InputOnly,
            _ => Role::Stream,
        };
        if args.get("input_only").is_some_and(|v| v == "1") {
            role = Role::InputOnly;
        }
        if args.get("remote_monitor").is_some_and(|v| v == "1") {
            role = Role::RemoteMonitor;
        }
        if role == Role::InputOnly
            && !h
                .config
                .read()
                .unwrap()
                .boolean("enable_input_only_mode", false)
        {
            bail!("remote input is disabled by the administrator");
        }
        let app_id = if resume {
            let game = h.current_app.lock().unwrap().as_ref().map(|a| a.id);
            if owner == remote::Owner::Monitor {
                role = Role::RemoteMonitor;
                2147483505
            } else if let Some(id) = game {
                id
            } else if h.monitors.lock().unwrap().contains_key(&client.uuid) {
                role = Role::RemoteMonitor;
                2147483505
            } else {
                bail!("no application or remote monitor to resume");
            }
        } else {
            requested
        };
        let app = h
            .apps
            .read()
            .unwrap()
            .iter()
            .find(|a| {
                a.id() == app_id
                    || a.aliases.contains(&app_id)
                    || args.get("appuuid").is_some_and(|id| {
                        a.extra.get("uuid").and_then(serde_json::Value::as_str) == Some(id)
                    })
            })
            .cloned();
        if role == Role::Stream && app.is_none() {
            bail!("application not found");
        }
        let app_id = app.as_ref().map_or(app_id, |a| a.id());
        let launch = Launch {
            id: uuid::Uuid::new_v4().to_string(),
            client,
            peer: connection.peer.ip(),
            app_id,
            key,
            key_id,
            ping: hex::encode(crypto::random::<8>()),
            connect_data: rand::random(),
            role,
            created: Instant::now(),
            rtsp_encrypted: args
                .get("corever")
                .is_some_and(|v| v.parse::<u32>().unwrap_or(0) >= 1),
            rtsp_counter: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(1)),
            rtsp_received: Default::default(),
            preparation: Default::default(),
            vrr_requested: args.get("vrr").is_some_and(|s| s == "1")
                || args.get("client_vrr").is_some_and(|s| s == "1")
                || args.get("clientVrrRequested").is_some_and(|s| s == "1"),
            host_audio: args.get("localAudioPlayMode").is_some_and(|s| s == "1"),
            requested_rate: args
                .get("mode")
                .and_then(|mode| mode.rsplit('x').next())
                .and_then(|rate| {
                    if rate.contains('.') {
                        butterpollo_core::framegen::Rate::parse(rate).ok()
                    } else {
                        rate.parse()
                            .ok()
                            .map(butterpollo_core::framegen::Rate::from_client)
                    }
                })
                .map_or(0, |rate| rate.0),
            audio_preparation: Default::default(),
            options: args.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        };
        let rtsp_port = h.config.read().unwrap().ports()?.rtsp;
        if !launch.rtsp_encrypted
            && crate::network::encryption_mode(&h.config.read().unwrap(), connection.peer.ip()) == 2
        {
            bail!("encrypted RTSP is required for this client address");
        }
        let scheme = if launch.rtsp_encrypted {
            "rtspenc"
        } else {
            "rtsp"
        };
        let id = launch.id.clone();
        // This client abandoned any earlier launch or stream in this role. Let
        // a stream finish its teardown first, so its display, audio and client
        // commands are released before the new launch prepares them again.
        let superseded = h
            .sessions
            .lock()
            .unwrap()
            .supersede(&launch.client.uuid, role);
        if !superseded.is_empty() {
            tracing::info!(client = %launch.client.name, "replacing this client's previous stream");
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline
                && superseded
                    .iter()
                    .any(|s| h.sessions.lock().unwrap().active.contains_key(&s.launch.id))
            {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let mut current = h.current_app.lock().unwrap();
        if role == Role::Stream && !resume && current.as_ref().is_some_and(|a| a.id != app_id) {
            if owner != remote::Owner::None {
                return Err(LaunchFailure(409, "Remote Input or Remote Monitor is active; launch Terminate before starting a different app").into());
            }
            let generation = &current.as_ref().unwrap().generation;
            if h.config
                .read()
                .unwrap()
                .boolean("remote_monitor_confirm_app_replacement", true)
                && !h.confirmations.lock().unwrap().confirm(
                    &launch.client.uuid,
                    remote::Confirmation::Replace,
                    generation,
                    app_id,
                    Instant::now(),
                )
            {
                return Err(LaunchFailure(410, "An app is already running. Launch this app again within 60 seconds to confirm that you want to close it.").into());
            }
            h.confirmations
                .lock()
                .unwrap()
                .clear(&launch.client.uuid, remote::Confirmation::Replace);
            h.sessions.lock().unwrap().stop_role(Role::Stream, None);
            current.take();
            drop(current);
            h.app_audio.lock().unwrap().take();
            h.app_display.lock().unwrap().clear();
            let deadline = Instant::now() + Duration::from_secs(10);
            while h
                .sessions
                .lock()
                .unwrap()
                .active
                .values()
                .any(|s| s.launch.role == Role::Stream)
            {
                if Instant::now() >= deadline {
                    bail!(
                        "previous game session is still releasing its resources; retry the launch"
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            current = h.current_app.lock().unwrap();
        }
        h.sessions.lock().unwrap().queue(launch.clone())?;
        if role != Role::InputOnly {
            let prepared = (|| -> Result<std::sync::Arc<crate::display_session::Ready>> {
                let config = crate::stream::effective_config(&h, &launch)?;
                let mode = args
                    .get("mode")
                    .map(String::as_str)
                    .unwrap_or(config.get("fallback_mode", "1920x1080x60"));
                let dimensions: Vec<_> = mode.split('x').collect();
                if dimensions.len() != 3 {
                    bail!("launch mode must be WIDTHxHEIGHTxFPS");
                }
                let rate = if dimensions[2].contains('.') {
                    butterpollo_core::framegen::Rate::parse(dimensions[2])?
                } else {
                    butterpollo_core::framegen::Rate::from_client(dimensions[2].parse()?)
                };
                let mut stream = butterpollo_core::rtsp::Negotiated {
                    width: dimensions[0].parse().context("invalid launch width")?,
                    height: dimensions[1].parse().context("invalid launch height")?,
                    fps: rate.rounded(),
                    rate_millihz: rate.0,
                    hdr: args.get("hdrMode").is_some_and(|v| v == "1"),
                    codec: 1,
                    vrr_low_latency: launch.vrr_requested,
                    ..Default::default()
                };
                butterpollo_core::stream_policy::apply_color(&mut stream, &config);
                stream.validate()?;
                if role == Role::Stream {
                    // Reuse only this client's own retained display; another
                    // client streaming the same app keeps its display.
                    let retained = h
                        .app_display
                        .lock()
                        .unwrap()
                        .get(&launch.client.uuid)
                        .map(|(lease, _)| lease.clone());
                    if let Some(lease) = retained
                        && lease.matches(&stream)
                    {
                        return Ok(lease);
                    }
                    h.app_display.lock().unwrap().remove(&launch.client.uuid);
                }
                crate::display_session::Ready::new(crate::display_session::Prepared::create(
                    &h, &launch, &stream, &config,
                )?)
            })();
            match prepared {
                Ok(prepared) => *launch.preparation.lock().unwrap() = Some(Box::new(prepared)),
                Err(error) => {
                    h.sessions.lock().unwrap().pending.remove(&id);
                    return Err(error);
                }
            }
        }
        if role != Role::InputOnly {
            let config = crate::stream::effective_config(&h, &launch)?;
            if config.boolean("stream_audio", true)
                && !(role == Role::RemoteMonitor
                    && config.boolean("remote_monitor_mute_audio", false))
            {
                let channels = args
                    .get("surroundAudioInfo")
                    .and_then(|s| s.parse::<u32>().ok())
                    .map_or(2, |v| v & 0xffff) as usize;
                let channels = if matches!(channels, 2 | 6 | 8) {
                    channels
                } else {
                    2
                };
                match butterpollo_windows::audio_route::Route::acquire(
                    &config,
                    &h.directory,
                    launch.host_audio,
                    channels,
                ) {
                    Ok(route) => {
                        *launch.audio_preparation.lock().unwrap() =
                            Some(Box::new(std::sync::Arc::new(route)))
                    }
                    Err(error) => {
                        tracing::warn!(%error, "audio route will be retried after stream negotiation")
                    }
                }
            }
        }
        if role == Role::Stream && current.is_none() {
            let mut process_args = args.clone();
            process_args.insert("clientName".into(), launch.client.name.clone());
            process_args.insert("clientUuid".into(), launch.client.uuid.clone());
            match crate::process::launch(
                &h,
                app.as_ref().context("application not found")?,
                &process_args,
            ) {
                Ok(running) => *current = Some(running),
                Err(e) => {
                    h.sessions.lock().unwrap().pending.remove(&id);
                    return Err(e);
                }
            }
        }
        if role == Role::Stream {
            *h.app_audio.lock().unwrap() = launch
                .audio_preparation
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|route| {
                    route.downcast_ref::<std::sync::Arc<butterpollo_windows::audio_route::Route>>()
                })
                .cloned();
            if let Some(lease) = launch
                .preparation
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|lease| {
                    lease.downcast_ref::<std::sync::Arc<crate::display_session::Ready>>()
                })
                .cloned()
            {
                h.app_display
                    .lock()
                    .unwrap()
                    .insert(launch.client.uuid.clone(), (lease, None));
            }
        }
        tracing::info!(
            client = %launch.client.name,
            app_id,
            role = ?role,
            mode = args.get("mode").map_or("", String::as_str),
            hdr = args.get("hdrMode").is_some_and(|v| v == "1"),
            resume,
            "Moonlight session launched"
        );
        let host = match connection.local.ip() {
            std::net::IpAddr::V4(ip) => ip.to_string(),
            std::net::IpAddr::V6(ip) => format!("[{ip}]"),
        };
        Ok((
            if resume { "resume" } else { "gamesession" }.into(),
            format!("{scheme}://{host}:{rtsp_port}"),
        ))
    })();
    match result {
        Ok((key, url)) => xml(
            200,
            &[(key.as_str(), "1".into()), ("sessionUrl0", url)],
            None,
        ),
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                app_id = requested,
                resume,
                "Moonlight session launch failed"
            );
            xml(
                error
                    .downcast_ref::<LaunchFailure>()
                    .map_or(503, |failure| failure.0),
                &[(if resume { "resume" } else { "gamesession" }, "0".into())],
                Some(error.to_string()),
            )
        }
    }
}
async fn cancel(State(h): State<Shared>, Extension(connection): Extension<Connection>) -> Response {
    let client = match authenticated(&h, &connection, 1 << 26) {
        Ok(client) => client,
        Err(e) => return xml(401, &[], Some(e.to_string())),
    };
    let _transition = h.launch_transition.lock().unwrap();
    if let Some(game) = remote_game(&h) {
        let remote_active = !h.monitors.lock().unwrap().is_empty()
            || h.sessions
                .lock()
                .unwrap()
                .active
                .values()
                .any(|s| !s.stopping() && s.launch.role != Role::Stream);
        if remote_active && game.owner != client.uuid {
            return xml(
                403,
                &[("cancel", "0".into())],
                Some("Only the game owner may cancel while remote sessions are active".into()),
            );
        }
    } else {
        return xml(200, &[("cancel", "1".into())], None);
    }
    h.sessions.lock().unwrap().stop_role(Role::Stream, None);
    h.stop_app();
    xml(200, &[("cancel", "1".into())], None)
}
async fn unpair(State(h): State<Shared>, Extension(connection): Extension<Connection>) -> Response {
    let result = authenticated(&h, &connection, 0).and_then(|c| {
        h.paired.write().unwrap().remove(&h.paired_path, &c.uuid)?;
        h.sessions.lock().unwrap().request_stop(Some(&c.uuid));
        crate::remote_display::disconnect(&h, Some(&c.uuid));
        Ok(())
    });
    match result {
        Ok(()) => xml(200, &[("unpaired", "1".into())], None),
        Err(e) => xml(401, &[], Some(e.to_string())),
    }
}
async fn appasset(
    State(h): State<Shared>,
    Extension(c): Extension<Connection>,
    Query(args): Query<Args>,
) -> Response {
    if authenticated(&h, &c, 1 << 24).is_err() {
        return xml(401, &[], None);
    }
    let app = args
        .get("appid")
        .and_then(|id| id.parse::<u32>().ok())
        .and_then(|id| {
            let id = remote_game(&h)
                .filter(|game| id == butterpollo_core::remote::running_game_id(game.app.id))
                .map_or(id, |game| game.app.id);
            h.apps
                .read()
                .unwrap()
                .iter()
                .find(|a| a.id() == id || a.aliases.contains(&id))
                .cloned()
        });
    let assets = h.assets.parent().unwrap_or(&h.assets);
    let control_art = args
        .get("appid")
        .and_then(|id| id.parse().ok())
        .and_then(|id| butterpollo_core::remote::identify(id, ""))
        .and_then(|control| match control {
            butterpollo_core::remote::Control::Resume => Some("resume.png"),
            butterpollo_core::remote::Control::DisconnectMonitor => {
                Some("disconnect-remote-monitor.png")
            }
            butterpollo_core::remote::Control::DisconnectInput => {
                Some("disconnect-remote-input.png")
            }
            butterpollo_core::remote::Control::Terminate => Some("terminate.png"),
            butterpollo_core::remote::Control::Monitor => Some("remote-monitor.png"),
            butterpollo_core::remote::Control::Input => Some("remote-input.png"),
            _ => None,
        });
    let path = control_art
        .map(|name| assets.join("remote-session").join(name))
        .or_else(|| app.map(|a| butterpollo_core::catalog::artwork(&a, assets)))
        .unwrap_or_else(|| assets.join("box.png"));
    if !tokio::fs::metadata(&path)
        .await
        .is_ok_and(|m| m.len() <= 16 * 1024 * 1024)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) if bytes.len() <= 16 * 1024 * 1024 => {
            let mime = if bytes.starts_with(b"\x89PNG") {
                "image/png"
            } else if bytes.starts_with(b"\xff\xd8") {
                "image/jpeg"
            } else {
                return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
            };
            ([(header::CONTENT_TYPE, mime)], bytes).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
fn clipboard_client(h: &Shared, c: &Connection, permission: u32, args: &Args) -> Result<()> {
    let client = authenticated(h, c, permission)?;
    if client.perm & ((1 << 25) | (1 << 26)) == 0 {
        bail!("view permission required");
    }
    if args.get("type").is_none_or(|v| v != "text") {
        bail!("only text clipboard data is supported");
    }
    if !h
        .sessions
        .lock()
        .unwrap()
        .active
        .values()
        .any(|s| s.launch.client.uuid == client.uuid && !s.stopping())
    {
        bail!("clipboard access requires an active session");
    }
    Ok(())
}
async fn clipboard_read(
    State(h): State<Shared>,
    Extension(c): Extension<Connection>,
    Query(args): Query<Args>,
) -> Response {
    if clipboard_client(&h, &c, 1 << 17, &args).is_err() {
        return StatusCode::FORBIDDEN.into_response();
    }
    match tokio::task::spawn_blocking(butterpollo_windows::clipboard::read).await {
        Ok(Ok(text)) => {
            ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
async fn clipboard_write(
    State(h): State<Shared>,
    Extension(c): Extension<Connection>,
    Query(args): Query<Args>,
    body: Bytes,
) -> Response {
    if clipboard_client(&h, &c, 1 << 16, &args).is_err() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(text) = std::str::from_utf8(&body).map(str::to_owned) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match tokio::task::spawn_blocking(move || butterpollo_windows::clipboard::write(&text)).await {
        Ok(Ok(())) => StatusCode::OK.into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
async fn bitrate(
    State(h): State<Shared>,
    Extension(c): Extension<Connection>,
    Query(args): Query<Args>,
) -> Response {
    let result = (|| -> Result<usize> {
        let client = authenticated(&h, &c, 1 << 25)?;
        let bitrate = args
            .get("bitrate")
            .or_else(|| args.get("bitrate_kbps"))
            .context("missing bitrate")?
            .parse::<u32>()?;
        if !(100..=2_000_000).contains(&bitrate) {
            bail!("invalid bitrate");
        }
        let sessions = h.sessions.lock().unwrap();
        let mut count = 0;
        for s in sessions.active.values() {
            if s.launch.client.uuid == client.uuid {
                s.bitrate
                    .store(bitrate, std::sync::atomic::Ordering::Release);
                count += 1;
            }
        }
        Ok(count)
    })();
    match result {
        Ok(n) => xml(200, &[("updated", n.to_string())], None),
        Err(e) => xml(400, &[], Some(e.to_string())),
    }
}
async fn abr(State(h): State<Shared>, Extension(c): Extension<Connection>) -> Response {
    if authenticated(&h, &c, 1 << 25).is_err() {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(json!({"supported":true,"min_bitrate_kbps":100,"max_bitrate_kbps":2_000_000}))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::stream_key_id;

    #[test]
    fn android_signed_stream_keys_preserve_the_wire_iv() {
        for (signed, unsigned) in [
            ("0", "0"),
            ("2147483647", "2147483647"),
            ("-2147483648", "2147483648"),
            ("-1", "4294967295"),
        ] {
            assert_eq!(
                stream_key_id(signed).unwrap().to_be_bytes(),
                stream_key_id(unsigned).unwrap().to_be_bytes()
            );
        }
        for invalid in ["", "-2147483649", "4294967296", "1.5", "0x123", "key"] {
            assert!(stream_key_id(invalid).is_err(), "{invalid}");
        }
    }
}
