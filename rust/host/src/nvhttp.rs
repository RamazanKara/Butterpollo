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
    let config = h.config.read().unwrap();
    let ports = config.ports().unwrap();
    let paired = authenticated(&h, &connection, 0).is_ok();
    let current = h
        .current_app
        .lock()
        .unwrap()
        .as_ref()
        .map(|a| a.id)
        .unwrap_or(0);
    xml(
        200,
        &[
            (
                "hostname",
                config.get("sunshine_name", "Butterpollo Rust").into(),
            ),
            ("appversion", "7.1.431.-1".into()),
            ("GfeVersion", "3.23.0.74".into()),
            ("uniqueid", h.paired.read().unwrap().unique_id.clone()),
            ("HttpsPort", ports.https.to_string()),
            ("ExternalPort", ports.http.to_string()),
            ("PairStatus", u8::from(paired).to_string()),
            ("currentgame", current.to_string()),
            (
                "state",
                if current == 0 {
                    "SUNSHINE_SERVER_FREE"
                } else {
                    "SUNSHINE_SERVER_BUSY"
                }
                .into(),
            ),
            ("LocalIP", connection.local.ip().to_string()),
            ("mac", "00:00:00:00:00:00".into()),
            ("MaxLumaPixelsHEVC", "1869449984".into()),
            (
                "ServerCodecModeSupport",
                h.codecs
                    .load(std::sync::atomic::Ordering::Acquire)
                    .to_string(),
            ),
            ("RustHostVersion", env!("CARGO_PKG_VERSION").into()),
        ],
        None,
    )
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
    let mut s = "<?xml version=\"1.0\"?><root status_code=\"200\">".to_owned();
    for app in h.apps.read().unwrap().iter() {
        s.push_str(&format!(
            "<App><AppTitle>{}</AppTitle><ID>{}</ID><IsHdrSupported>0</IsHdrSupported></App>",
            escape(&app.name),
            app.id()
        ));
    }
    for (id, title) in butterpollo_core::remote::tiles() {
        let allowed = match butterpollo_core::remote::identify(id, "").unwrap() {
            butterpollo_core::remote::Control::Terminate => client.perm & (1 << 18) != 0,
            butterpollo_core::remote::Control::Resume => client.perm & (1 << 25) != 0,
            _ => client.perm & (1 << 26) != 0,
        };
        if allowed {
            s.push_str(&format!(
                "<App><AppTitle>{}</AppTitle><ID>{id}</ID><IsHdrSupported>1</IsHdrSupported></App>",
                escape(title)
            ));
        }
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
    let control = remote::identify(requested, args.get("appuuid").map_or("", String::as_str));
    let permission = match control {
        Some(Control::Terminate) => 1 << 18,
        Some(Control::Resume) => 1 << 25,
        _ if resume => 1 << 25,
        _ => 1 << 26,
    };
    let client = match authenticated(&h, &connection, permission) {
        Ok(c) => c,
        Err(e) => return xml(401, &[], Some(e.to_string())),
    };
    match control {
        Some(Control::Terminate) => {
            h.sessions.lock().unwrap().stop_role(Role::Stream, None);
            h.current_app.lock().unwrap().take();
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
    let resume = resume || control == Some(Control::Resume);
    let result = (|| -> Result<(String, String)> {
        let key: [u8; 16] = hex::decode(args.get("rikey").context("missing stream key")?)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid stream key length"))?;
        let key_id = args
            .get("rikeyid")
            .context("missing stream key ID")?
            .parse()?;
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
        let app_id = if resume {
            let game = h.current_app.lock().unwrap().as_ref().map(|a| a.id);
            if let Some(id) = game {
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
        };
        let rtsp_port = h.config.read().unwrap().ports()?.rtsp;
        let scheme = if launch.rtsp_encrypted {
            "rtspenc"
        } else {
            "rtsp"
        };
        let id = launch.id.clone();
        let mut current = h.current_app.lock().unwrap();
        if role == Role::Stream && !resume && current.as_ref().is_some_and(|a| a.id != app_id) {
            bail!("an application is already running");
        }
        h.sessions.lock().unwrap().queue(launch)?;
        if role == Role::Stream && current.is_none() {
            match crate::process::launch(&h, app.as_ref().context("application not found")?, &args)
            {
                Ok(running) => *current = Some(running),
                Err(e) => {
                    h.sessions.lock().unwrap().pending.remove(&id);
                    return Err(e);
                }
            }
        }
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
        Err(e) => xml(
            503,
            &[(if resume { "resume" } else { "gamesession" }, "0".into())],
            Some(e.to_string()),
        ),
    }
}
async fn cancel(State(h): State<Shared>, Extension(connection): Extension<Connection>) -> Response {
    if let Err(e) = authenticated(&h, &connection, 1 << 18) {
        return xml(401, &[], Some(e.to_string()));
    }
    h.sessions.lock().unwrap().stop_role(Role::Stream, None);
    h.current_app.lock().unwrap().take();
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
            h.apps
                .read()
                .unwrap()
                .iter()
                .find(|a| a.id() == id || a.aliases.contains(&id))
                .cloned()
        });
    let assets = h.assets.parent().unwrap_or(&h.assets);
    let path = app
        .map(|a| butterpollo_core::catalog::artwork(&a, assets))
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
        if !(100..=500000).contains(&bitrate) {
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
    axum::Json(json!({"supported":true,"min_bitrate_kbps":100,"max_bitrate_kbps":500000}))
        .into_response()
}
