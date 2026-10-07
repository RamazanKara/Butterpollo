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
/// Watching a stream needs the view or the launch permission, as in
/// Vibepollo: a device that may launch a game may also resume it.
const VIEW: u32 = (1 << 25) | (1 << 26);
fn authenticated_viewer(h: &Shared, connection: &Connection) -> Result<Client> {
    let client = authenticated(h, connection, 0)?;
    if client.perm & VIEW == 0 {
        bail!("client permission denied");
    }
    Ok(client)
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
        .route("/pair/", get(pair).post(pair))
        // Plain HTTP answers too, but only HTTPS can identify a client.
        .route("/unpair", get(unpair).post(unpair));
    let r = if https {
        r.route("/applist", get(applist))
            .route("/launch", get(launch))
            .route("/resume", get(resume))
            .route("/cancel", get(cancel))
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
    h.request_codec_probe();
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
async fn pair(
    State(h): State<Shared>,
    Extension(connection): Extension<Connection>,
    Query(args): Query<Args>,
) -> Response {
    match do_pair(h, &args, connection.peer.ip().to_canonical()).await {
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
/// Whether a pairing request has to wait for another device's: Moonlight's
/// apps share one unique ID, so only the certificate tells devices apart. The
/// same device asking again replaces its own request, as after a wrong PIN.
fn another_device_pairing(existing: Option<(&str, bool)>, certificate: &str) -> bool {
    existing.is_some_and(|(theirs, active)| active && theirs != certificate)
}
async fn do_pair(h: Shared, args: &Args, peer: std::net::IpAddr) -> Result<Vec<(String, String)>> {
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
        // Another device's request still waiting for its PIN, or in the
        // seconds of its handshake, is not replaced: the PIN typed for it
        // would reach this device instead.
        let waiting = h.pins.lock().unwrap().get(&id).map(|p| {
            (
                p.certificate.clone(),
                p.created.elapsed() < Duration::from_secs(300) && !p.sender.is_closed(),
            )
        });
        let handshaking = h.pairings.lock().unwrap().sessions.get(&id).map(|p| {
            (
                p.certificate.clone(),
                p.created.elapsed() < Duration::from_secs(30),
            )
        });
        if another_device_pairing(
            waiting.as_ref().map(|(c, a)| (c.as_str(), *a)),
            &certificate,
        ) || another_device_pairing(
            handshaking.as_ref().map(|(c, a)| (c.as_str(), *a)),
            &certificate,
        ) {
            bail!("another device is pairing with this PC; finish or cancel that first");
        }
        // A new certificate request replaces this device's unfinished
        // pairing, as in the previous host. Moonlight abandons a pairing after
        // a wrong PIN without telling the host, and its apps share one unique
        // ID, so the stale session refused every later attempt until restart.
        h.pairings.lock().unwrap().sessions.remove(&id);
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
            let mut pair = Pairing::new(id, name, certificate, &salt, &pin)?;
            pair.peer = Some(peer);
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
        let created = Instant::now();
        {
            let mut pins = h.pins.lock().unwrap();
            pins.retain(|_, p| p.created.elapsed() < Duration::from_secs(300));
            // Replacing an older request ends its wait for the PIN.
            pins.remove(&id);
            if pins.len() >= 32 {
                bail!("pairing request limit reached");
            }
            pins.insert(
                id.clone(),
                PendingPin {
                    name: name.clone(),
                    certificate: certificate.clone(),
                    created,
                    sender,
                },
            );
        }
        tracing::info!(client=%name,"Pairing PIN required in the web interface");
        butterpollo_windows::tray::notify(
            "Pairing request",
            &format!("{name} wants to pair. Enter the PIN it shows in the Butterpollo console."),
        );
        let response = tokio::time::timeout(Duration::from_secs(300), receiver).await;
        {
            // Only this request's entry: a newer request may have replaced it.
            let mut pins = h.pins.lock().unwrap();
            if pins.get(&id).is_some_and(|p| p.created == created) {
                pins.remove(&id);
            }
        }
        let (pin, name) = response
            .context("PIN entry timed out")?
            .context("pairing cancelled")?;
        let mut pair = Pairing::new(id, name, certificate, &salt, &pin)?;
        pair.peer = Some(peer);
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
    // A malformed step is refused before the pairing is touched: from any
    // other device it must not end someone else's pairing.
    enum Step {
        Challenge(Vec<u8>),
        Response(Vec<u8>),
        Secret(Vec<u8>),
    }
    let step = if let Some(v) = args.get("clientchallenge") {
        if v.len() != 32 {
            bail!("invalid challenge length");
        }
        Step::Challenge(hex::decode(v)?)
    } else if let Some(v) = args.get("serverchallengeresp") {
        if v.len() != 64 {
            bail!("invalid challenge response length");
        }
        Step::Response(hex::decode(v)?)
    } else if let Some(v) = args.get("clientpairingsecret") {
        if v.len() != 544 {
            bail!("invalid pairing secret length");
        }
        Step::Secret(hex::decode(v)?)
    } else {
        bail!("unknown pairing phase")
    };
    let mut pairings = h.pairings.lock().unwrap();
    pairings.expire();
    let p = pairings
        .sessions
        .get_mut(&id)
        .context("no pending pairing")?;
    if p.peer.is_some_and(|started| started != peer) {
        bail!("this pairing was started from another address");
    }
    let result = (|| -> Result<Vec<(String, String)>> {
        match &step {
            Step::Challenge(challenge) => {
                let response = p.client_challenge(&h.identity, challenge)?;
                Ok(vec![
                    ("paired".into(), "1".into()),
                    ("challengeresponse".into(), hex::encode(response)),
                ])
            }
            Step::Response(response) => {
                let response = p.server_response(&h.identity, response)?;
                Ok(vec![
                    ("paired".into(), "1".into()),
                    ("pairingsecret".into(), hex::encode(response)),
                ])
            }
            Step::Secret(secret) => {
                p.finish(secret)?;
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
                butterpollo_windows::tray::notify(
                    "Device paired",
                    &format!("{} can now stream from this PC.", p.name),
                );
                Ok(vec![("paired".into(), "1".into())])
            }
        }
    })();
    if result.is_err() || matches!(step, Step::Secret(_)) {
        pairings.sessions.remove(&id);
    }
    result
}
async fn applist(
    State(h): State<Shared>,
    Extension(connection): Extension<Connection>,
) -> Response {
    let client = match authenticated(&h, &connection, 0) {
        Ok(c) => c,
        Err(e) => return xml(401, &[], Some(e.to_string())),
    };
    if !client.allows(1 << 24) {
        // As in Vibepollo: one entry that tells the user what to change.
        return ([(header::CONTENT_TYPE, "application/xml")], "<?xml version=\"1.0\"?><root status_code=\"200\"><App><IsHdrSupported>0</IsHdrSupported><AppTitle>Permission denied - enable &quot;List applications&quot; for this device in the host's Web UI</AppTitle><UUID></UUID><IDX>0</IDX><ID>114514</ID></App></root>").into_response();
    }
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
                Some(Control::Resume | Control::RunningGame) => VIEW,
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
        h.codecs.load(std::sync::atomic::Ordering::Acquire)
            & (0x200 | 0x20000 | 0x2000000 | 0x4000000)
            != 0,
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
// Launching prepares displays and audio and runs prep commands, for seconds
// to minutes: on the blocking pool, not on the workers that serve every
// other request.
async fn launch(
    State(h): State<Shared>,
    Extension(connection): Extension<Connection>,
    Query(args): Query<Args>,
) -> Response {
    blocking(move || start(h, connection, args, false)).await
}
async fn resume(
    State(h): State<Shared>,
    Extension(connection): Extension<Connection>,
    Query(args): Query<Args>,
) -> Response {
    blocking(move || start(h, connection, args, true)).await
}
async fn blocking(work: impl FnOnce() -> Response + Send + 'static) -> Response {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
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
    let authorized = if permission == 1 << 25 {
        authenticated_viewer(&h, &connection)
    } else {
        authenticated(&h, &connection, permission)
    };
    let client = match authorized {
        Ok(c) => c,
        Err(error) => {
            tracing::warn!(%error, app_id = requested, resume, "Moonlight launch authorization failed");
            return xml(401, &[], Some(error.to_string()));
        }
    };
    h.request_codec_probe();
    let _transition = h.launch_transition.lock().unwrap();
    if crate::updater::installing(&h) {
        return xml(
            503,
            &[],
            Some("Butterpollo is installing an update. Reconnect shortly.".into()),
        );
    }
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
    // The reply names the endpoint Moonlight called. Android and iOS read
    // <gamesession> from /launch and fail without it, also when the launch
    // joins the running game.
    let reply = if resume { "resume" } else { "gamesession" };
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
            preparing: Default::default(),
            warnings: Default::default(),
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
            let previous = current.take();
            drop(current);
            // Stopping the app waits for it to exit, never under the lock
            // that every serverinfo and app list request takes.
            drop(previous);
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
        // Preparing displays, audio and the app's own commands can take many
        // seconds; serverinfo and the app list read current_app meanwhile.
        // Launches and stops stay serialized by launch_transition.
        drop(current);
        // Preparing can outlast the pending launch's 30 s (prep commands may
        // run for minutes); it expires only from the reply on, on every path.
        struct Preparing(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Preparing {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        launch
            .preparing
            .store(true, std::sync::atomic::Ordering::Release);
        let preparing = Preparing(launch.preparing.clone());
        h.sessions.lock().unwrap().queue(launch.clone())?;
        if role != Role::InputOnly {
            let prepared = (|| -> Result<crate::display_session::StreamPreparation> {
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
                    // Encoders need even sizes. Moonlight rounds only the
                    // height, and only later; a 2556x1179 phone was refused.
                    width: dimensions[0]
                        .parse::<u32>()
                        .context("invalid launch width")?
                        & !1,
                    height: dimensions[1]
                        .parse::<u32>()
                        .context("invalid launch height")?
                        & !1,
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
                        return lease.resume(&h.directory, &config, launch.warnings.clone());
                    }
                    h.app_display.lock().unwrap().remove(&launch.client.uuid);
                }
                crate::display_session::Ready::prepare(crate::display_session::Prepared::create(
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
        // Prep commands can run for minutes: the app starts without the lock
        // that every serverinfo, app list and asset request takes. Launches
        // and stops stay serialized by launch_transition, so no other app is
        // installed meanwhile.
        if role == Role::Stream && h.current_app.lock().unwrap().is_none() {
            let mut process_args = args.clone();
            process_args.insert("clientName".into(), launch.client.name.clone());
            process_args.insert("clientUuid".into(), launch.client.uuid.clone());
            match crate::process::launch(
                &h,
                app.as_ref().context("application not found")?,
                &process_args,
            ) {
                Ok(running) => {
                    let mut current = h.current_app.lock().unwrap();
                    if current.is_none() {
                        *current = Some(running);
                    } else {
                        drop(current);
                        tracing::warn!(
                            "another application started meanwhile; this launch's app is stopped"
                        );
                        drop(running);
                    }
                }
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
                .and_then(|lease| lease.downcast_ref::<crate::display_session::StreamPreparation>())
                .map(|prepared| prepared.display.clone())
            {
                h.app_display
                    .lock()
                    .unwrap()
                    .insert(launch.client.uuid.clone(), (lease, None));
            }
        }
        // The client has 30 s to connect from now on.
        match h.sessions.lock().unwrap().pending.get_mut(&id) {
            Some(pending) => pending.created = Instant::now(),
            None => bail!("the launch was replaced by a newer one from this device"),
        }
        drop(preparing);
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
        Ok((reply.into(), format!("{scheme}://{host}:{rtsp_port}")))
    })();
    match result {
        Ok((key, url)) => xml(
            200,
            &[
                (key.as_str(), "1".into()),
                ("sessionUrl0", url),
                (
                    "VirtualDisplayDriverReady",
                    butterpollo_windows::display::virtual_display_available().to_string(),
                ),
            ],
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
                &[(reply, "0".into())],
                Some(error.to_string()),
            )
        }
    }
}
async fn cancel(State(h): State<Shared>, Extension(connection): Extension<Connection>) -> Response {
    // Stopping waits for the app to exit and runs its undo commands.
    blocking(move || cancel_app(h, connection)).await
}
fn cancel_app(h: Shared, connection: Connection) -> Response {
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
    // Only a paired client's own certificate can unpair it; anything else is
    // answered like Vibepollo, with nothing removed.
    let Ok(client) = authenticated(&h, &connection, 0) else {
        return xml(200, &[("unpaired", "0".into())], None);
    };
    let removed = h
        .paired
        .write()
        .unwrap()
        .remove(&h.paired_path, &client.uuid);
    if let Err(e) = removed {
        return xml(500, &[("unpaired", "0".into())], Some(e.to_string()));
    }
    h.sessions.lock().unwrap().request_stop(Some(&client.uuid));
    crate::remote_display::disconnect(&h, Some(&client.uuid));
    xml(200, &[("unpaired", "1".into())], None)
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
    // Vibepollo's reply: the applied bitrate, 0 on failure.
    let failed = |code, message: &str| xml(code, &[("bitrate", "0".into())], Some(message.into()));
    let client = match authenticated_viewer(&h, &c) {
        Ok(client) => client,
        Err(e) => return failed(403, &e.to_string()),
    };
    let Some(requested) = args
        .get("bitrate")
        .or_else(|| args.get("bitrate_kbps"))
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
    else {
        return failed(400, "Missing or invalid bitrate parameter");
    };
    let applied =
        butterpollo_core::stream_policy::runtime_bitrate_kbps(&h.config.read().unwrap(), requested);
    let sessions = h.sessions.lock().unwrap();
    let mut count = 0;
    for s in sessions.active.values() {
        if s.launch.client.uuid == client.uuid {
            butterpollo_core::stream_policy::report_bitrate(
                &s.launch.warnings,
                requested,
                applied,
                "max_bitrate and the 500 Mbps runtime cap",
            );
            s.bitrate
                .store(applied, std::sync::atomic::Ordering::Release);
            count += 1;
            let minimum = butterpollo_core::pyrowave::minimum_kbps(
                s.config.width,
                s.config.height,
                s.config.fps_millihz(),
            );
            let recommended = butterpollo_core::pyrowave::recommended_kbps(
                s.config.width,
                s.config.height,
                s.config.fps_millihz(),
            );
            if s.config.codec == 3 && applied < minimum {
                tracing::warn!(
                    bitrate_kbps = applied,
                    minimum_kbps = minimum,
                    recommended_kbps = recommended,
                    "PyroWave bitrate is too low: severe detail loss is likely. Raise the bitrate in Moonlight with network headroom, or use HEVC or AV1"
                );
            } else if s.config.codec == 3 && applied < recommended {
                tracing::warn!(
                    bitrate_kbps = applied,
                    minimum_kbps = minimum,
                    recommended_kbps = recommended,
                    "PyroWave bitrate is below recommended: text and textures may lose detail. Quality depends on the picture; raise the bitrate in Moonlight with network headroom, or use HEVC or AV1"
                );
            }
        }
    }
    if count == 0 {
        return failed(404, "No active session for this client");
    }
    tracing::info!(client = %client.name, requested, applied, "client set the stream bitrate");
    xml(
        200,
        &[
            ("bitrate", applied.to_string()),
            ("updated", count.to_string()),
        ],
        None,
    )
}
async fn abr(State(h): State<Shared>, Extension(c): Extension<Connection>) -> Response {
    if authenticated_viewer(&h, &c).is_err() {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    // The host has no adaptive bitrate of its own; clients that see this run
    // their own controller and apply it through /bitrate.
    axum::Json(json!({"supported":false,"version":1,"features":["runtime_bitrate"]}))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::stream_key_id;
    #[test]
    fn another_device_cannot_take_over_a_pairing_in_progress() {
        use super::another_device_pairing as blocked;
        // Nothing pending, or this device asking again (after a wrong PIN).
        assert!(!blocked(None, "mine"));
        assert!(!blocked(Some(("mine", true)), "mine"));
        // Another device whose request is still waiting or handshaking.
        assert!(blocked(Some(("theirs", true)), "mine"));
        // Another device that gave up: its stale request is replaced.
        assert!(!blocked(Some(("theirs", false)), "mine"));
    }

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
