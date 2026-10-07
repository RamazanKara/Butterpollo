use crate::{state::Shared, tls::Connection};
use anyhow::Context;
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::Engine;
use butterpollo_core::{
    auth, crypto, state,
    state::{App, Credentials},
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn response(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}
fn error(status: StatusCode, message: &str) -> Response {
    response(status, json!({"status":false,"error":message}))
}
pub(crate) fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|c| {
            let (k, v) = c.trim().split_once('=')?;
            if k == name { Some(v.to_owned()) } else { None }
        })
}
pub(crate) fn access(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned)
        .or_else(|| cookie(headers, "__Host-apollo_session"))
}
/// Sign-in attempts per address over the last minute. The login form and
/// Basic credentials share it, so neither lets a peer guess the password at
/// full speed. This PC is never throttled, and IPv6 counts per /64, the
/// block one LAN host can take addresses from.
struct SignIns {
    attempts: std::collections::HashMap<std::net::IpAddr, (Instant, u32)>,
}
static SIGN_INS: std::sync::LazyLock<std::sync::Mutex<SignIns>> = std::sync::LazyLock::new(|| {
    std::sync::Mutex::new(SignIns {
        attempts: Default::default(),
    })
});
impl SignIns {
    const LIMIT: u32 = 10;
    const WINDOW: Duration = Duration::from_secs(60);
    const TRACKED: usize = 1024;
    fn key(peer: std::net::IpAddr) -> Option<std::net::IpAddr> {
        let peer = peer.to_canonical();
        if peer.is_loopback() {
            return None;
        }
        Some(match peer {
            std::net::IpAddr::V6(address) => {
                std::net::Ipv6Addr::from(u128::from(address) & (u128::MAX << 64)).into()
            }
            v4 => v4,
        })
    }
    /// Count an attempt; false when the address has to wait.
    fn attempt(&mut self, peer: std::net::IpAddr, now: Instant) -> bool {
        let Some(key) = Self::key(peer) else {
            return true;
        };
        self.attempts
            .retain(|_, (since, _)| now.saturating_duration_since(*since) < Self::WINDOW);
        // Full: forget the oldest address instead of refusing every new
        // one, which would let a flood of addresses lock everyone out.
        if self.attempts.len() >= Self::TRACKED
            && !self.attempts.contains_key(&key)
            && let Some(oldest) = self
                .attempts
                .iter()
                .min_by_key(|(_, (since, _))| *since)
                .map(|(address, _)| *address)
        {
            self.attempts.remove(&oldest);
        }
        let (_, count) = self.attempts.entry(key).or_insert((now, 0));
        *count += 1;
        *count <= Self::LIMIT
    }
    fn succeeded(&mut self, peer: std::net::IpAddr) {
        if let Some(key) = Self::key(peer) {
            self.attempts.remove(&key);
        }
    }
}
/// The app of a cover request: /api/apps/ID/cover or /api/covers/ID. Not by
/// slicing: "/api/apps/cover" both starts and ends right, and a panic aborts
/// the host.
fn cover_id(path: &str) -> Option<&str> {
    path.strip_prefix("/api/apps/")
        .and_then(|rest| rest.strip_suffix("/cover"))
        .or_else(|| path.strip_prefix("/api/covers/"))
        .filter(|id| !id.is_empty())
}
/// Whether the request is signed in: a browser session, or Basic
/// credentials from `peer`, which count against its sign-in attempts.
pub(crate) fn authenticated(
    h: &Shared,
    headers: &HeaderMap,
    peer: Option<std::net::IpAddr>,
) -> bool {
    if let Some(token) = access(headers) {
        let mut sessions = h.web_sessions.lock().unwrap();
        let Some(key) = crate::web_sessions::resolve_hash(&sessions, &token) else {
            return false;
        };
        let Some(session) = sessions
            .get_mut(&key)
            .filter(|s| s.expires > Instant::now())
        else {
            return false;
        };
        let wall = crate::web_sessions::now();
        if wall.saturating_sub(session.last_seen) >= 300 {
            let previous = session.last_seen;
            session.last_seen = wall;
            if let Err(error) = h.save_web_sessions(&sessions) {
                sessions.get_mut(&key).unwrap().last_seen = previous;
                tracing::debug!(%error, "browser session activity could not be saved");
            }
        }
        return true;
    }
    if let Some(encoded) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        && let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded)
        && let Ok(text) = std::str::from_utf8(&decoded)
        && let Some((user, pass)) = text.split_once(':')
    {
        let peer = peer.unwrap_or(std::net::Ipv4Addr::UNSPECIFIED.into());
        if !SIGN_INS.lock().unwrap().attempt(peer, Instant::now()) {
            return false;
        }
        let verified = h
            .credentials
            .read()
            .unwrap()
            .as_ref()
            .is_some_and(|c| c.verifies(user, pass));
        if verified {
            SIGN_INS.lock().unwrap().succeeded(peer);
        }
        return verified;
    }
    false
}
fn token_authenticated(h: &Shared, headers: &HeaderMap, path: &str, method: &str) -> bool {
    let Some(secret) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    let credentials = h.credentials.read().unwrap();
    let Some(credentials) = credentials.as_ref() else {
        return false;
    };
    auth::read(&h.aliases.lock().unwrap())
        .is_ok_and(|tokens| auth::permits(&tokens, secret, &credentials.username, path, method))
}
fn token_catalog() -> Vec<auth::Scope> {
    [
        ("/api/config", &["GET", "POST", "PATCH"][..]),
        ("/api/metadata", &["GET"][..]),
        ("/api/apps", &["GET", "POST"][..]),
        ("/api/apps/[^/]+", &["DELETE"][..]),
        ("/api/apps/[^/]+/cover", &["GET"][..]),
        ("/api/apps/close", &["POST"][..]),
        ("/api/apps/launch", &["POST"][..]),
        ("/api/apps/reorder", &["POST"][..]),
        ("/api/apps/rtx_hdr/live", &["POST"][..]),
        ("/api/clients/list", &["GET"][..]),
        ("/api/clients/update", &["POST"][..]),
        ("/api/clients/unpair", &["POST"][..]),
        ("/api/clients/disconnect", &["POST"][..]),
        ("/api/session/status", &["GET"][..]),
        ("/api/rtsp/sessions", &["GET"][..]),
        ("/api/display-devices", &["GET"][..]),
        ("/api/framegen/edid-refresh", &["GET"][..]),
        ("/api/clients/display-layout", &["GET", "PUT"][..]),
        ("/api/clients/hdr-profiles", &["GET"][..]),
        ("/api/clients/unpair-all", &["POST"][..]),
        ("/api/frame-limiter/status", &["GET"][..]),
        ("/api/rtss/status", &["GET"][..]),
        ("/api/health/vulkan-hdr-layer", &["GET"][..]),
        ("/api/health/vulkan-hdr-layer/register", &["POST"][..]),
        ("/api/health/crashdump", &["GET"][..]),
        ("/api/health/crashdump/dismiss", &["POST"][..]),
        ("/api/display/golden_status", &["GET"][..]),
        ("/api/display/export_golden", &["POST"][..]),
        ("/api/display/restore_golden", &["POST"][..]),
        ("/api/display/golden", &["DELETE"][..]),
        ("/api/display/terminate_virtual", &["POST"][..]),
        ("/api/reset-display-device-persistence", &["POST"][..]),
        ("/api/updates", &["GET"][..]),
        ("/api/updates/check", &["POST"][..]),
        ("/api/updates/install", &["POST"][..]),
        ("/api/updates/cancel", &["POST"][..]),
        ("/api/covers/upload", &["POST"][..]),
        ("/api/covers/[0-9]+", &["GET"][..]),
        ("/api/logs", &["GET"][..]),
        ("/api/logs/export", &["GET"][..]),
        ("/api/logs/export_crash", &["GET"][..]),
        ("/api/logs/export_crash/manifest", &["GET"][..]),
        ("/api/pin", &["POST"][..]),
        ("/api/otp", &["POST"][..]),
        ("/api/clients/pending", &["GET"][..]),
        ("/api/logs/tail", &["GET"][..]),
        ("/api/steam/status", &["GET"][..]),
        ("/api/steam/games", &["GET"][..]),
        ("/api/steam/force_sync", &["POST"][..]),
        ("/api/steam/launch", &["POST"][..]),
        ("/api/playnite/status", &["GET"][..]),
        ("/api/playnite/games", &["GET"][..]),
        ("/api/playnite/categories", &["GET"][..]),
        ("/api/playnite/install", &["POST"][..]),
        ("/api/playnite/uninstall", &["POST"][..]),
        ("/api/playnite/force_sync", &["POST"][..]),
        ("/api/apps/purge_autosync", &["POST"][..]),
        ("/api/lossless_scaling/status", &["GET"][..]),
        ("/api/restart", &["POST"][..]),
        ("/api/quit", &["POST"][..]),
        ("/api/password", &["POST"][..]),
    ]
    .into_iter()
    .map(|(path, methods)| auth::Scope {
        path: path.into(),
        methods: methods.iter().map(|m| (*m).into()).collect(),
    })
    .collect()
}
pub fn router(h: Shared) -> Router {
    Router::new()
        .route(
            "/api/{*path}",
            get(api).post(api).patch(api).put(api).delete(api),
        )
        .route(
            "/console/action",
            axum::routing::post(crate::console::action),
        )
        .route("/console.css", get(crate::console::stylesheet))
        .route("/favicon.svg", get(crate::console::favicon))
        .fallback(site)
        // Uploaded cover images are the largest requests.
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(h.clone(), guard))
        .with_state(h)
}
/// Allowed sources for the web app: covers come from IGDB and the cover
/// search reads LizardByte's game database.
const APP_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https://images.igdb.com; connect-src 'self' https://raw.githubusercontent.com; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";
/// The console: the web app when it is installed, else the server-rendered
/// pages.
async fn site(
    state: State<Shared>,
    connection: Extension<Connection>,
    method: Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response {
    let root = state.0.assets.clone();
    if root.join("index.html").is_file() {
        return app_file(&root, &method, uri.path());
    }
    crate::console::page(state, connection, method, uri, headers).await
}
/// A file of the web app; the app's own page paths get index.html.
fn app_file(root: &std::path::Path, method: &Method, path: &str) -> Response {
    if !matches!(*method, Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let relative = path.trim_start_matches('/');
    let safe = !relative.is_empty()
        && relative.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':'])
        });
    let found = safe
        .then(|| root.join(relative))
        .filter(|file| file.is_file());
    let asset = relative.starts_with("assets/");
    if asset && found.is_none() {
        // A page built before an upgrade asks for files that are gone; HTML
        // in their place would fail as a script.
        return StatusCode::NOT_FOUND.into_response();
    }
    let file = found.unwrap_or_else(|| root.join("index.html"));
    let Ok(bytes) = std::fs::read(&file) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let kind = match file.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };
    let mut response = ([(header::CONTENT_TYPE, kind)], bytes).into_response();
    let headers = response.headers_mut();
    if asset {
        // Built files carry a content hash in their names.
        headers.insert(
            header::CACHE_CONTROL,
            "public, max-age=31536000, immutable".parse().unwrap(),
        );
    }
    if kind.starts_with("text/html") {
        headers.insert(header::CONTENT_SECURITY_POLICY, APP_POLICY.parse().unwrap());
    }
    response
}
async fn guard(State(h): State<Shared>, mut request: Request, next: Next) -> Response {
    // origin_web_ui_allowed: pc, lan (default) or wan, as in Vibepollo.
    if let Some(connection) = request.extensions().get::<Connection>()
        && crate::network::reach(connection.peer.ip())
            > crate::network::web_reach(&h.config.read().unwrap())
    {
        tracing::info!(peer = %connection.peer, "web interface request from outside the allowed network refused");
        return error(StatusCode::FORBIDDEN, "Forbidden");
    }
    let console_form = request.uri().path() == "/console/action";
    let (method, path) = if console_form {
        if request.method() != Method::POST {
            return error(StatusCode::METHOD_NOT_ALLOWED, "POST required");
        }
        if !request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(';').next() == Some("application/x-www-form-urlencoded"))
        {
            return error(StatusCode::BAD_REQUEST, "form content type required");
        }
        let (mut parts, body) = request.into_parts();
        let bytes = match axum::body::to_bytes(body, 1024 * 1024).await {
            Ok(bytes) => bytes,
            Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "form exceeds the limit"),
        };
        let fields: crate::console::Fields =
            url::form_urlencoded::parse(&bytes).into_owned().collect();
        let route = match crate::console::route(&fields) {
            Ok(route) => route,
            Err(message) => return error(StatusCode::BAD_REQUEST, &message),
        };
        let Some(csrf) = fields
            .get("_csrf")
            .and_then(|v| v.parse::<axum::http::HeaderValue>().ok())
        else {
            return error(StatusCode::BAD_REQUEST, "CSRF token required");
        };
        parts.headers.insert("X-CSRF-Token", csrf);
        request = Request::from_parts(parts, axum::body::Body::from(bytes));
        route
    } else {
        (request.method().clone(), request.uri().path().to_owned())
    };
    let path = path.as_str();
    let public = matches!(
        path,
        "/api/auth/login"
            | "/api/auth/refresh"
            | "/api/auth/status"
            | "/api/csrf-token"
            | "/api/configLocale"
    );
    let fresh_password = path == "/api/password" && h.credentials.read().unwrap().is_none();
    let api_token = token_authenticated(&h, request.headers(), path, method.as_str());
    if fresh_password
        && !request
            .extensions()
            .get::<Connection>()
            .is_some_and(|c| c.peer.ip().is_loopback())
    {
        return error(
            StatusCode::FORBIDDEN,
            "initial credentials must be set from this computer",
        );
    }
    if path.starts_with("/api/")
        && !public
        && !fresh_password
        && !api_token
        && !authenticated(
            &h,
            request.headers(),
            request
                .extensions()
                .get::<Connection>()
                .map(|c| c.peer.ip()),
        )
    {
        return error(StatusCode::UNAUTHORIZED, "authentication required");
    }
    if !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS) {
        if let Some(origin) = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
        {
            let allowed = request
                .headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .map(|host| format!("https://{host}"));
            let configured = h
                .config
                .read()
                .unwrap()
                .get("csrf_allowed_origins", "[]")
                .to_owned();
            let origins = butterpollo_core::config::Config::parse(&format!(
                "csrf_allowed_origins = {configured}\n"
            ))
            .map(|c| c.list("csrf_allowed_origins"))
            .unwrap_or_default();
            if allowed.as_deref() != Some(origin)
                && !origins.iter().any(|allowed| allowed == origin)
            {
                return error(StatusCode::FORBIDDEN, "request origin is not allowed");
            }
        }
        if path == "/api/auth/login"
            || fresh_password
            || (console_form && access(request.headers()).is_none())
        {
            let expected = cookie(request.headers(), "__Host-apollo_anon_csrf");
            let got = request
                .headers()
                .get("X-CSRF-Token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if (console_form && expected.is_none())
                || expected
                    .is_some_and(|expected| !crypto::equal(expected.as_bytes(), got.as_bytes()))
            {
                return error(StatusCode::BAD_REQUEST, "CSRF token required");
            }
        }
        if !public
            && !fresh_password
            && !api_token
            && let Some(token) = access(request.headers())
        {
            let sessions = h.web_sessions.lock().unwrap();
            let expected = crate::web_sessions::find(&sessions, &token).map(|s| s.csrf.as_str());
            let got = request
                .headers()
                .get("X-CSRF-Token")
                .and_then(|v| v.to_str().ok());
            if expected.is_none()
                || got.is_none()
                || !crypto::equal(expected.unwrap().as_bytes(), got.unwrap().as_bytes())
            {
                return error(StatusCode::BAD_REQUEST, "CSRF token required");
            }
        }
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("X-Content-Type-Options", "nosniff".parse().unwrap());
    headers.insert("X-Frame-Options", "DENY".parse().unwrap());
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert("no-store".parse().unwrap());
    response
}
fn issued(
    h: &Shared,
    username: String,
    remember_me: bool,
    headers: &HeaderMap,
    connection: &Connection,
    previous: Option<crate::web_sessions::WebSession>,
) -> Response {
    let username = h
        .credentials
        .read()
        .unwrap()
        .as_ref()
        .filter(|c| c.username.eq_ignore_ascii_case(&username))
        .map(|c| c.username.clone())
        .unwrap_or(username);
    let ttl = h
        .config
        .read()
        .unwrap()
        .integer("session_token_ttl_seconds", 7200)
        .clamp(60, 604800) as u64;
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .chars()
        .take(512)
        .collect();
    let (access, refresh, csrf, refresh_ttl) = match h.new_web_session(
        username,
        remember_me,
        user_agent,
        connection.peer.ip().to_string(),
        previous,
    ) {
        Ok(tokens) => tokens,
        Err(e) => {
            if e.is::<crate::web_sessions::Rotated>() {
                return error(StatusCode::UNAUTHORIZED, "refresh token expired");
            }
            tracing::error!(error=%e, "browser session could not be persisted");
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "browser session could not be saved",
            );
        }
    };
    let ttl = ttl.min(refresh_ttl);
    let mut r = response(
        StatusCode::OK,
        json!({"status":true,"access_token":access,"refresh_token":refresh,"csrf_token":csrf,"expires_in":ttl,"refresh_expires_in":refresh_ttl,"remember_me":remember_me,"redirect":"/"}),
    );
    for (name, value, lifetime) in [
        ("__Host-apollo_session", access, ttl),
        ("__Host-apollo_refresh", refresh, refresh_ttl),
    ] {
        let expiry = if remember_me {
            format!("; Max-Age={lifetime}")
        } else {
            String::new()
        };
        r.headers_mut().append(
            header::SET_COOKIE,
            format!("{name}={value}; Path=/; HttpOnly; SameSite=Strict; Secure{expiry}")
                .parse()
                .unwrap(),
        );
    }
    r
}
pub(crate) fn refresh_browser(
    h: &Shared,
    headers: &HeaderMap,
    connection: &Connection,
) -> Option<Response> {
    let token = cookie(headers, "__Host-apollo_refresh")?;
    let previous = h
        .web_sessions
        .lock()
        .unwrap()
        .values()
        .find(|s| {
            crypto::matches_hash(token.as_bytes(), &s.refresh) && s.refresh_expires > Instant::now()
        })
        .cloned()?;
    Some(issued(
        h,
        previous.username.clone(),
        previous.remember_me,
        headers,
        connection,
        Some(previous),
    ))
}
pub(crate) async fn api(
    State(h): State<Shared>,
    Extension(connection): Extension<Connection>,
    method: Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path();
    let data: Value = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return error(StatusCode::BAD_REQUEST, "invalid JSON"),
        }
    };
    let text = |k: &str| data.get(k).and_then(Value::as_str).unwrap_or("");
    if method == Method::POST && path == "/api/covers/upload" {
        let result = async {
            let key = text("key");
            if key.is_empty()
                || key.len() > 128
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                anyhow::bail!("invalid cover key");
            }
            if let Some(data) = data.get("data").and_then(Value::as_str) {
                // The request body limit bounds the size.
                let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
                if bytes.len() < 24 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                    anyhow::bail!("cover must be PNG");
                }
                let path = h.directory.join("covers").join(format!("{key}.png"));
                state::atomic_write(&path, &bytes)?;
                return Ok(json!({"status":true,"path":path}));
            }
            let url = url::Url::parse(text("url"))?;
            if url.scheme() != "https"
                || url.host_str() != Some("images.igdb.com")
                || !url.path().starts_with("/igdb/image/upload/")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.port().is_some()
            {
                anyhow::bail!("cover URL must use the IGDB image service");
            }
            let client = reqwest::Client::builder()
                .https_only(true)
                .timeout(std::time::Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()?;
            let mut response = client.get(url).send().await?.error_for_status()?;
            let mut bytes = vec![];
            while let Some(chunk) = response.chunk().await? {
                if bytes.len() + chunk.len() > 16 * 1024 * 1024 {
                    anyhow::bail!("cover exceeds the limit");
                }
                bytes.extend_from_slice(&chunk);
            }
            if bytes.len() < 24 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                anyhow::bail!("cover must be PNG");
            }
            let path = h.directory.join("covers").join(format!("{key}.png"));
            state::atomic_write(&path, &bytes)?;
            Ok::<_, anyhow::Error>(json!({"status":true,"path":path}))
        }
        .await;
        return match result {
            Ok(value) => Json(value).into_response(),
            Err(err) => error(StatusCode::BAD_REQUEST, &err.to_string()),
        };
    }
    if let Some(action) = path.strip_prefix("/api/steam/") {
        // Reading the libraries and converting covers waits on the disk.
        let h = h.clone();
        let appid = |value: Option<&Value>| -> Option<u32> {
            match value? {
                Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
                Value::String(s) => s.trim().parse().ok(),
                _ => None,
            }
        };
        let query: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
                .into_owned()
                .collect();
        let task = match (method.as_str(), action) {
            ("GET", "status") => {
                tokio::task::spawn_blocking(move || Ok(crate::steam::status(&h))).await
            }
            ("GET", "games") => {
                let id = query.get("appid").and_then(|v| v.parse().ok());
                tokio::task::spawn_blocking(move || crate::steam::games(&h, id)).await
            }
            ("POST", "force_sync") => {
                tokio::task::spawn_blocking(move || {
                    crate::steam::sync(&h).map(|outcome| {
                        json!({"status":true,"changed":outcome.changed,"game_count":outcome.games,"importable_game_count":outcome.importable})
                    })
                })
                .await
            }
            ("POST", "launch") => {
                let Some(id) = appid(data.get("appid")).or_else(|| appid(data.get("steam_id")))
                else {
                    return error(StatusCode::BAD_REQUEST, "appid required");
                };
                tokio::task::spawn_blocking(move || crate::steam::launch(&h, id)).await
            }
            _ => return error(StatusCode::BAD_REQUEST, "unknown API endpoint"),
        };
        return match task {
            Ok(Ok(value)) => Json(value).into_response(),
            Ok(Err(err)) => error(StatusCode::BAD_REQUEST, &format!("{err:#}")),
            Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
        };
    }
    if let Some(action) = path.strip_prefix("/api/playnite/") {
        let h = h.clone();
        let task = match (method.as_str(), action) {
            ("GET", "status") => {
                tokio::task::spawn_blocking(move || Ok(crate::playnite::status(&h))).await
            }
            ("GET", "games") => tokio::task::spawn_blocking(crate::playnite::games).await,
            ("GET", "categories") => tokio::task::spawn_blocking(crate::playnite::categories).await,
            ("POST", "install") => {
                tokio::task::spawn_blocking(move || {
                    crate::playnite::install_plugin(&h).map(|folder| {
                        json!({"status":true,"path":folder,
                               "restart_required":butterpollo_windows::playnite::running().is_some()})
                    })
                })
                .await
            }
            ("POST", "uninstall") => {
                tokio::task::spawn_blocking(|| crate::playnite::uninstall_plugin().map(|()| json!({"status":true}))).await
            }
            ("POST", "force_sync") => {
                tokio::task::spawn_blocking(move || {
                    crate::playnite::sync(&h)
                        .map(|outcome| json!({"status":true,"changed":outcome.changed,"game_count":outcome.games}))
                })
                .await
            }
            _ => return error(StatusCode::BAD_REQUEST, "unknown API endpoint"),
        };
        return match task {
            Ok(Ok(value)) => Json(value).into_response(),
            Ok(Err(err)) => error(StatusCode::BAD_REQUEST, &format!("{err:#}")),
            Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
        };
    }
    if method == Method::GET && path == "/api/lossless_scaling/status" {
        let candidate = url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .find(|(key, _)| key == "path")
            .map(|(_, value)| value.into_owned());
        let configured = h
            .config
            .read()
            .unwrap()
            .get("lossless_scaling_path", "")
            .to_owned();
        let checked = candidate.unwrap_or_else(|| configured.clone());
        let result = tokio::task::spawn_blocking(move || {
            let resolved = butterpollo_windows::lossless::program(&checked);
            let status = match (&resolved, checked.trim().is_empty()) {
                (Some(_), _) => "detected",
                (None, true) => "not-configured",
                (None, false) => "path-not-found",
            };
            json!({"status": status, "configured_path": configured, "checked_path": checked,
                   "resolved_path": resolved, "using_configured_path": !configured.trim().is_empty()})
        })
        .await;
        return match result {
            Ok(value) => Json(value).into_response(),
            Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
        };
    }
    if method == Method::POST && path == "/api/apps/purge_autosync" {
        let mut removed = 0;
        return match crate::steam::update_apps(&h, |apps| {
            removed = butterpollo_core::playnite::purge(apps);
            removed > 0
        }) {
            Ok(_) => Json(json!({"status":true,"removed":removed})).into_response(),
            Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{err:#}")),
        };
    }
    if method == Method::GET && path == "/api/logs/export_crash" {
        let h = h.clone();
        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<std::fs::File> {
            use std::os::windows::fs::OpenOptionsExt;
            let path = crate::maintenance::bundle(&h)?;
            Ok(std::fs::OpenOptions::new()
                .access_mode(0x80000000 | 0x00010000)
                .share_mode(1 | 4)
                .custom_flags(0x04000000)
                .open(path)?)
        })
        .await;
        return match result {
            Ok(Ok(file)) => (
                [
                    (header::CONTENT_TYPE, "application/zip"),
                    (
                        header::CONTENT_DISPOSITION,
                        "attachment; filename=butterpollo-support.zip",
                    ),
                ],
                axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(
                    tokio::fs::File::from_std(file),
                )),
            )
                .into_response(),
            Ok(Err(err)) => error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
            Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
        };
    }
    if method == Method::GET && path == "/api/logs/tail" {
        use std::io::{Read, Seek};
        let query: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
                .into_owned()
                .collect();
        let offset: i64 = query
            .get("offset")
            .and_then(|v| v.parse().ok())
            .unwrap_or(-1);
        let max: u64 = query
            .get("max")
            .and_then(|v| v.parse().ok())
            .unwrap_or(256 * 1024)
            .clamp(1024, 4 * 1024 * 1024);
        let result = (|| -> std::io::Result<Value> {
            let mut file = match std::fs::File::open(crate::maintenance::log_path(&h)) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(
                        json!({"status":true,"offset":0,"size":0,"reset":offset != 0,"text":""}),
                    );
                }
                Err(e) => return Err(e),
            };
            let size = file.metadata()?.len();
            // A negative offset asks for the tail; an offset past the end
            // means the log was rotated.
            let reset = offset < 0 || offset as u64 > size;
            let start = if reset {
                size.saturating_sub(max)
            } else {
                offset as u64
            };
            file.seek(std::io::SeekFrom::Start(start))?;
            let mut bytes = vec![];
            file.take(max).read_to_end(&mut bytes)?;
            // End on a line break so no line is split between two reads.
            if start + (bytes.len() as u64) < size
                && let Some(end) = bytes.iter().rposition(|b| *b == b'\n')
            {
                bytes.truncate(end + 1);
            }
            Ok(json!({
                "status": true,
                "offset": start + bytes.len() as u64,
                "size": size,
                "reset": reset,
                "text": String::from_utf8_lossy(&bytes),
            }))
        })();
        return match result {
            Ok(value) => axum::Json(value).into_response(),
            Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        };
    }
    if method == Method::GET && matches!(path, "/api/logs" | "/api/logs/export") {
        use std::io::{Read, Seek};
        let result = (|| -> std::io::Result<String> {
            let mut file = std::fs::File::open(crate::maintenance::log_path(&h))?;
            let length = file.metadata()?.len();
            file.seek(std::io::SeekFrom::Start(
                length.saturating_sub(8 * 1024 * 1024),
            ))?;
            let mut bytes = vec![];
            file.take(8 * 1024 * 1024).read_to_end(&mut bytes)?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        })();
        let mut result = (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            result.unwrap_or_default(),
        )
            .into_response();
        if path.ends_with("/export") {
            result.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                "attachment; filename=butterpollo.log".parse().unwrap(),
            );
        }
        return result;
    }
    if method == Method::GET
        && ((path.starts_with("/api/apps/") && path.ends_with("/cover"))
            || path.starts_with("/api/covers/"))
    {
        let Some(id) = cover_id(path) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let app = h
            .apps
            .read()
            .unwrap()
            .iter()
            .find(|a| {
                a.extra.get("uuid").and_then(Value::as_str) == Some(id) || a.id().to_string() == id
            })
            .cloned();
        let Some(app) = app else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let path = butterpollo_core::catalog::artwork(&app, h.assets.parent().unwrap_or(&h.assets));
        if tokio::fs::metadata(&path)
            .await
            .is_ok_and(|m| m.len() <= 16 * 1024 * 1024)
            && let Ok(bytes) = tokio::fs::read(path).await
        {
            return ([(header::CONTENT_TYPE, "image/png")], bytes).into_response();
        }
        return StatusCode::NOT_FOUND.into_response();
    }
    if path == "/api/auth/status" {
        let configured = h.credentials.read().unwrap().is_some();
        let authenticated = authenticated(&h, &headers, Some(connection.peer.ip()));
        return Json(json!({"authenticated":authenticated,"credentials_configured":configured,"login_required":configured&&!authenticated,"status":true})).into_response();
    }
    if path == "/api/auth/login" {
        if method != Method::POST {
            return error(StatusCode::METHOD_NOT_ALLOWED, "POST required");
        }
        if !SIGN_INS
            .lock()
            .unwrap()
            .attempt(connection.peer.ip(), Instant::now())
        {
            return error(StatusCode::TOO_MANY_REQUESTS, "try again in one minute");
        }
        let username = text("username");
        if h.credentials
            .read()
            .unwrap()
            .as_ref()
            .is_some_and(|c| c.verifies(username, text("password")))
        {
            SIGN_INS.lock().unwrap().succeeded(connection.peer.ip());
            let remember = data.get("remember_me").is_some_and(|v| {
                v.as_bool()
                    .unwrap_or_else(|| matches!(v.as_str(), Some("true" | "1" | "on")))
            });
            return issued(&h, username.into(), remember, &headers, &connection, None);
        }
        return error(StatusCode::UNAUTHORIZED, "invalid username or password");
    }
    if path == "/api/auth/refresh" {
        if method != Method::POST {
            return error(StatusCode::METHOD_NOT_ALLOWED, "POST required");
        }
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Refresh "))
            .map(str::to_owned)
            .or_else(|| cookie(&headers, "__Host-apollo_refresh"))
            .unwrap_or_else(|| text("refresh_token").into());
        let previous = {
            let sessions = h.web_sessions.lock().unwrap();
            sessions
                .iter()
                .find(|(_, s)| {
                    crypto::matches_hash(token.as_bytes(), &s.refresh)
                        && s.refresh_expires > Instant::now()
                })
                .map(|(_, s)| s.clone())
        };
        return match previous {
            Some(s) => issued(
                &h,
                s.username.clone(),
                s.remember_me,
                &headers,
                &connection,
                Some(s),
            ),
            None => error(StatusCode::UNAUTHORIZED, "refresh token expired"),
        };
    }
    if path == "/api/auth/logout" {
        if let Some(token) = access(&headers) {
            let mut sessions = h.web_sessions.lock().unwrap();
            let mut next = sessions.clone();
            if let Some(key) = crate::web_sessions::resolve_hash(&next, &token) {
                next.remove(&key);
            }
            if let Err(e) = h.save_web_sessions(&next) {
                return error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
            }
            *sessions = next;
        }
        let mut r = Json(json!({"status":true})).into_response();
        for name in ["__Host-apollo_session", "__Host-apollo_refresh"] {
            r.headers_mut().append(
                header::SET_COOKIE,
                format!("{name}=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0")
                    .parse()
                    .unwrap(),
            );
        }
        return r;
    }
    if path == "/api/csrf-token" {
        if method != Method::GET {
            return error(StatusCode::METHOD_NOT_ALLOWED, "GET required");
        }
        let token = access(&headers);
        let csrf = token
            .and_then(|t| {
                crate::web_sessions::find(&h.web_sessions.lock().unwrap(), &t)
                    .map(|s| s.csrf.clone())
            })
            .unwrap_or_default();
        if !csrf.is_empty() {
            return Json(json!({"status":true,"csrf_token":csrf,"token":csrf})).into_response();
        }
        let csrf = hex::encode(crypto::random::<32>());
        let mut result =
            Json(json!({"status":true,"csrf_token":csrf,"token":csrf})).into_response();
        result.headers_mut().insert(header::SET_COOKIE, format!("__Host-apollo_anon_csrf={csrf}; Path=/; HttpOnly; SameSite=Strict; Secure; Max-Age=3600").parse().unwrap());
        return result;
    }
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let path = uri.path();
        let text = |k: &str| data.get(k).and_then(Value::as_str).unwrap_or("");
        Ok(match (method.as_str(), path) {
            ("GET", "/api/updates") => crate::updater::status(&h),
            ("POST", "/api/updates/install") => { crate::updater::queue(&h, false)?; json!({"status":true}) },
            ("POST", "/api/updates/cancel") => { crate::updater::cancel(&h)?; json!({"status":true}) },
            ("POST", "/api/updates/check") => {
                crate::maintenance::trigger_update(&h);
                json!({"status":true})
            }
            ("GET", "/api/health/crashdump") => crate::maintenance::crash_status(&h)?,
            ("POST", "/api/health/crashdump/dismiss") => {
                crate::maintenance::dismiss_crash(&h, &data)?
            }
            ("GET", "/api/logs/export_crash/manifest") => crate::maintenance::bundle_manifest(&h),
            ("GET", "/api/display/golden_status") => crate::maintenance::golden_status(
                &h,
                uri.query().is_some_and(|q| q.contains("compare_current=1")),
            )?,
            ("POST", "/api/display/export_golden") => crate::maintenance::capture_golden(&h)?,
            ("POST", "/api/display/restore_golden") => crate::maintenance::restore_golden(&h)?,
            ("POST", "/api/reset-display-device-persistence") => {
                if !h.sessions.lock().unwrap().active.is_empty()
                    || !h.monitors.lock().unwrap().is_empty()
                {
                    anyhow::bail!(
                        "disconnect active sessions before resetting display persistence"
                    );
                }
                butterpollo_windows::display_recovery::reset()?;
                json!({"status":true})
            }
            ("DELETE", "/api/display/golden") => {
                let path = h.directory.join("display-baseline-rust.json");
                let exists = path.exists();
                if exists {
                    std::fs::remove_file(path)?;
                }
                state::atomic_write(
                    &h.directory.join("display-baseline-disabled"),
                    b"legacy baseline import disabled\n",
                )?;
                json!({"status":true,"deleted":exists})
            }
            ("POST", "/api/display/terminate_virtual") => {
                crate::runtime::release_displays(&h);
                json!({"status":true})
            }
            ("GET", "/api/clients/display-layout") => crate::remote_display::snapshot(&h)?,
            ("PUT", "/api/clients/display-layout") => crate::remote_display::save(&h, &data)?,
            ("GET", "/api/rtss/status" | "/api/frame-limiter/status") => {
                crate::maintenance::integration_status(&h)
            }
            ("GET", "/api/health/vulkan-hdr-layer") => butterpollo_windows::vulkan::status(
                h.config.read().unwrap().boolean("vulkan_hdr_layer", true),
            ),
            ("POST", "/api/health/vulkan-hdr-layer/register") => {
                butterpollo_windows::vulkan::register(true)?;
                butterpollo_windows::vulkan::status(
                    h.config.read().unwrap().boolean("vulkan_hdr_layer", true),
                )
            }
            ("GET", "/api/auth/sessions") => {
                let sessions = h.web_sessions.lock().unwrap();
                let current = crate::web_sessions::resolve_hash(&sessions, &access(&headers).unwrap_or_default());
                let sessions: Vec<_> = sessions
                    .iter()
                    .filter(|(_, s)| s.refresh_expires > Instant::now())
                    .map(|(token, s)| {
                        let mut row = crate::web_sessions::record(token, s);
                        let object = row.as_object_mut().unwrap();
                        object.remove("refresh_token_hash");
                        object.remove("rotation_id");
                        object.remove("hash");
                        object.insert("id".into(), json!(token));
                        object.insert(
                            "current".into(),
                            json!(current.as_ref().is_some_and(|current| crypto::equal(token.as_bytes(), current.as_bytes()))),
                        );
                        row
                    })
                    .collect();
                json!({"status":true,"sessions":sessions})
            }
            ("DELETE", p) if p.starts_with("/api/auth/sessions/") => {
                let hash = &p[19..];
                let mut sessions = h.web_sessions.lock().unwrap();
                let mut next = sessions.clone();
                next.retain(|token, _| !token.eq_ignore_ascii_case(hash));
                h.save_web_sessions(&next)?;
                *sessions = next;
                json!({"status":true,"deleted":true})
            }
            ("GET", "/api/token/routes") => json!({"status":true,"routes":token_catalog()}),
            ("GET", "/api/tokens") => {
                json!({"status":true,"tokens":auth::read(&h.aliases.lock().unwrap())?})
            }
            ("POST", "/api/token") => {
                let scopes: Vec<auth::Scope> =
                    serde_json::from_value(data.get("scopes").cloned().unwrap_or(Value::Null))?;
                let username = h
                    .credentials
                    .read()
                    .unwrap()
                    .as_ref()
                    .map(|c| c.username.clone())
                    .ok_or_else(|| anyhow::anyhow!("credentials required"))?;
                let (secret, token) = auth::issue(username, scopes, &token_catalog())?;
                let mut document = h.aliases.lock().unwrap();
                let mut next = document.clone();
                let mut tokens = auth::read(&next)?;
                if tokens.len() >= 256 {
                    anyhow::bail!("API token limit reached");
                }
                tokens.push(token);
                next["root"]["api_tokens"] = serde_json::to_value(tokens)?;
                state::write_json(&h.aliases_path, &next)?;
                *document = next;
                json!({"status":true,"token":secret})
            }
            ("DELETE", p) if p.starts_with("/api/token/") => {
                let hash = &p[11..];
                let mut document = h.aliases.lock().unwrap();
                let mut next = document.clone();
                let mut tokens = auth::read(&next)?;
                tokens.retain(|t| !t.hash.eq_ignore_ascii_case(hash));
                next["root"]["api_tokens"] = serde_json::to_value(tokens)?;
                state::write_json(&h.aliases_path, &next)?;
                *document = next;
                json!({"status":true})
            }
            ("GET", "/api/config") => {
                let c = h.config.read().unwrap();
                let mut v = c.json();
                v["status"] = json!(true);
                v["platform"] = json!("windows");
                v["version"] = json!(env!("CARGO_PKG_VERSION"));
                v
            }
            ("POST" | "PATCH", "/api/config") => {
                let mut object = data
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("configuration must be an object"))?
                    .clone();
                for key in ["status", "platform", "version"] {
                    object.remove(key);
                }
                let mut config = h.config.write().unwrap();
                let mut next = config.clone();
                next.update(&object)?;
                state::atomic_write(&h.config_path, next.text().as_bytes())?;
                let warning =
                    butterpollo_windows::vulkan::reconcile(next.boolean("vulkan_hdr_layer", true))
                        .err()
                        .map(|e| e.to_string());
                *config = next;
                drop(config);
                h.metadata.lock().unwrap().take();
                json!({"status":true,"restart_required":true,"warning":warning})
            }
            ("GET", "/api/configLocale") => {
                json!({"status":true,"locale":h.config.read().unwrap().get("locale","en")})
            }
            ("GET", "/api/meta" | "/api/metadata") => {
                let shared = h.clone();
                (|| -> anyhow::Result<Value> {
                    let mut cached=shared.metadata.lock().unwrap();
                    if let Some((checked,value))=cached.as_ref() && checked.elapsed()<Duration::from_secs(5) { return Ok(value.clone()); }
                    let _com=butterpollo_windows::capture::ComGuard::new()?;
                    let codecs=shared.codecs.load(std::sync::atomic::Ordering::Acquire);
                    let probing=shared.probing_codecs.load(std::sync::atomic::Ordering::Acquire);
                    let virtual_display=butterpollo_windows::display::virtual_display_status();
                    let capable=virtual_display["capable"].as_bool().unwrap_or(false);
                    let audio=butterpollo_windows::audio_route::endpoints();
                    let displays=butterpollo_windows::capture::displays();
                    let config=shared.config.read().unwrap().clone();
                    let port=config.ports()?.http;
                    let address = |host: String| if port == 47989 { host } else { format!("{host}:{port}") };
                    let addresses = butterpollo_windows::net::lan_addresses().unwrap_or_default().into_iter().map(address).collect::<Vec<_>>();
                    let pc_address = addresses.first().cloned().unwrap_or_else(|| address(std::env::var("COMPUTERNAME").unwrap_or_default()));
                    let value=json!({"status":true,"platform":"windows","version":env!("CARGO_PKG_VERSION"),"branch":"codex/butterpollo-rust","host_name":crate::network::host_name(&config),"pc_address":pc_address,"pc_addresses":addresses,"paired_devices":shared.paired.read().unwrap().clients.len(),"encoder_status":{"state":if probing {"checking"} else if codecs == 0 {"failed"} else {"ready"},"h264":codecs&1!=0,"hevc":codecs&0x100!=0,"av1":codecs&0x10000!=0,"pyrowave":codecs&0x800000!=0},"capture_status":{"configured_backend":config.get("capture","auto"),"virtual_display_configured":config.virtual_display_mode(butterpollo_windows::display::windows_11())!="disabled","displays":displays.as_ref().ok(),"error":displays.as_ref().err().map(|e|e.to_string())},"virtual_display":virtual_display,"audio_sinks":audio.as_ref().ok(),"audio_error":audio.as_ref().err().map(|e|e.to_string()),"audio_enabled":config.boolean("stream_audio",true),"features":{"rust_host":true,"hdr":true,"truehdr_runtime":butterpollo_windows::truehdr::available(),"pyrowave":codecs&0x800000!=0,"virtual_display":capable},"credentials_exists":shared.credentials.read().unwrap().is_some()});
                    *cached=Some((Instant::now(),value.clone())); Ok(value)
                })()?
            }
            ("GET", "/api/apps") => {
                let mut d = h.app_document.read().unwrap().clone();
                d["apps"] = serde_json::to_value(&*h.apps.read().unwrap())?;
                d["status"] = json!(true);
                d
            }
            ("POST", "/api/apps") => {
                let mut app: App = serde_json::from_value(data.clone())?;
                if app.name.trim().is_empty() {
                    anyhow::bail!("application name required");
                }
                let mut tuning = serde_json::Map::new();
                for &key in crate::stream::RTX_KEYS {
                    if let Some(value) = app
                        .extra
                        .get(&key.replace('_', "-"))
                        .filter(|value| !value.is_null() && value.as_str() != Some(""))
                    {
                        tuning.insert(key.into(), value.clone());
                    }
                    if let Some(value) = app
                        .extra
                        .get("config-overrides")
                        .and_then(|v| v.get(key))
                        .filter(|value| !value.is_null() && value.as_str() != Some(""))
                    {
                        tuning.insert(key.into(), value.clone());
                    }
                }
                h.config.read().unwrap().clone().update(&tuning)?;
                if !app.extra.contains_key("uuid") {
                    app.extra
                        .insert("uuid".into(), json!(uuid::Uuid::new_v4().to_string()));
                }
                let uuid = app.extra["uuid"].clone();
                let mut apps = h.apps.write().unwrap();
                let mut next = apps.clone();
                if let Some(i) = next.iter().position(|a| a.extra.get("uuid") == Some(&uuid)) {
                    next[i] = app;
                } else {
                    next.push(app);
                }
                h.assign_apps(&mut next)?;
                let mut doc = h.app_document.read().unwrap().clone();
                doc["apps"] = serde_json::to_value(&next)?;
                state::write_json(&h.apps_path, &doc)?;
                *apps = next;
                *h.app_document.write().unwrap() = doc;
                json!({"status":true,"uuid":uuid})
            }
            ("POST", "/api/apps/delete") => {
                delete_app(&h, text("uuid"))?;
                json!({"status":true})
            }
            ("POST", "/api/apps/reorder") => {
                let order = data["order"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("order must be an array"))?;
                let mut apps = h.apps.write().unwrap();
                let next = butterpollo_core::catalog::reorder(&apps, order);
                let mut document = h.app_document.read().unwrap().clone();
                document["apps"] = serde_json::to_value(&next)?;
                state::write_json(&h.apps_path, &document)?;
                *apps = next;
                *h.app_document.write().unwrap() = document;
                json!({"status":true})
            }
            ("POST", "/api/apps/rtx_hdr/live") => {
                let values = match data.get("config-overrides") {
                    Some(values) => values
                        .as_object()
                        .ok_or_else(|| anyhow::anyhow!("config-overrides must be an object"))?
                        .clone(),
                    None => Default::default(),
                };
                json!({"status":true,"applied":h.update_live_rtx(text("uuid"),&values)?})
            }
            ("POST", "/api/apps/close") => {
                h.sessions
                    .lock()
                    .unwrap()
                    .stop_role(butterpollo_core::session::Role::Stream, None);
                h.stop_app();
                json!({"status":true})
            }
            ("POST", "/api/apps/launch") => {
                let _transition = h.launch_transition.lock().unwrap();
                if crate::updater::installing(&h) { anyhow::bail!("Butterpollo is installing an update"); }
                let uuid = text("uuid");
                let app = h
                    .apps
                    .read()
                    .unwrap()
                    .iter()
                    .find(|a| a.extra.get("uuid").and_then(Value::as_str) == Some(uuid))
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("application not found"))?;
                let mut current = h.current_app.lock().unwrap();
                if current.is_some() {
                    anyhow::bail!("application already running");
                }
                *current = Some(crate::process::launch(&h, &app, &Default::default())?);
                json!({"status":true})
            }
            ("GET", "/api/clients/list") => {
                let connected = h
                    .sessions
                    .lock()
                    .unwrap()
                    .active
                    .values()
                    .filter(|s| !s.stopping())
                    .map(|s| s.launch.client.uuid.clone())
                    .collect::<std::collections::HashSet<_>>();
                let clients: Vec<_> = h
                    .paired
                    .read()
                    .unwrap()
                    .clients
                    .iter()
                    .map(|c| {
                        let mut v = serde_json::to_value(c).unwrap();
                        v.as_object_mut().unwrap().remove("cert");
                        v["connected"] = json!(connected.contains(&c.uuid));
                        v
                    })
                    .collect();
                json!({"status":true,"clients":clients,"named_certs":clients})
            }
            ("GET", "/api/clients/hdr-profiles") => {
                let path = std::path::PathBuf::from(
                    std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into()),
                )
                .join("System32/spool/drivers/color");
                let mut profiles: Vec<_> = std::fs::read_dir(path)?.filter_map(Result::ok).filter(|e| e.path().extension().and_then(|s|s.to_str()).is_some_and(|s|s.eq_ignore_ascii_case("icc") || s.eq_ignore_ascii_case("icm"))).take(1024).map(|e|json!({"filename":e.file_name().to_string_lossy(),"added_ms":e.metadata().ok().and_then(|m|m.created().ok()).and_then(|t|t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d|d.as_millis()).unwrap_or(0)})).collect();
                profiles.sort_by_key(|p| std::cmp::Reverse(p["added_ms"].as_u64().unwrap_or(0)));
                json!({"status":true,"profiles":profiles})
            }
            ("POST", "/api/clients/unpair") => {
                h.sessions.lock().unwrap().request_stop(Some(text("uuid")));
                crate::remote_display::disconnect(&h, Some(text("uuid")));
                h.paired
                    .write()
                    .unwrap()
                    .remove(&h.paired_path, text("uuid"))?;
                json!({"status":true})
            }
            ("POST", "/api/clients/unpair-all") => {
                h.sessions.lock().unwrap().request_stop(None);
                crate::remote_display::disconnect(&h, None);
                let mut state = h.paired.write().unwrap();
                let mut next = state.clone();
                next.clients.clear();
                next.save(&h.paired_path)?;
                *state = next;
                json!({"status":true})
            }
            ("POST", "/api/clients/disconnect") => {
                h.sessions.lock().unwrap().request_stop(Some(text("uuid")));
                crate::remote_display::disconnect(&h, Some(text("uuid")));
                json!({"status":true})
            }
            ("POST", "/api/clients/update") => {
                let mut state = h.paired.write().unwrap();
                let mut next = state.clone();
                let c = next
                    .clients
                    .iter_mut()
                    .find(|c| c.uuid == text("uuid"))
                    .ok_or_else(|| anyhow::anyhow!("client not found"))?;
                let previous_perm = c.perm;
                if let Some(n) = data.get("name").and_then(Value::as_str) {
                    c.name = n.into();
                }
                if let Some(n) = data.get("perm").and_then(Value::as_u64) {
                    c.perm = u32::try_from(n)? & 0x071f1f00;
                }
                if let Some(v) = data.get("enabled").and_then(Value::as_bool) {
                    c.enabled = v;
                }
                for (k, v) in data
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("client update must be an object"))?
                {
                    if !matches!(k.as_str(), "uuid" | "cert" | "perm" | "name" | "enabled") {
                        // null clears a per-device setting.
                        if v.is_null() {
                            c.extra.remove(k);
                        } else {
                            c.extra.insert(k.clone(), v.clone());
                        }
                    }
                }
                let revoked = data.get("enabled").and_then(Value::as_bool) == Some(false)
                    || data
                        .get("perm")
                        .and_then(Value::as_u64)
                        .is_some_and(|p| previous_perm & !(p as u32) != 0);
                next.save(&h.paired_path)?;
                *state = next;
                drop(state);
                // Other changes apply from the device's next stream; disabling
                // it or removing permissions ends what it is doing now.
                if revoked {
                    h.sessions.lock().unwrap().request_stop(Some(text("uuid")));
                    crate::remote_display::disconnect(&h, Some(text("uuid")));
                }
                json!({"status":true,"disconnected":revoked})
            }
            ("GET", "/api/clients/pending") => {
                let pins = h.pins.lock().unwrap();
                json!({"status":true,"requests":pins
                    .iter()
                    .filter(|(_, p)| p.created.elapsed() < Duration::from_secs(300))
                    .map(|(id, p)| json!({"uniqueid":id,"name":p.name,"age_seconds":p.created.elapsed().as_secs()}))
                    .collect::<Vec<_>>()})
            }
            ("GET", "/api/rtsp/sessions") => {
                json!({"status":true,"sessions":h.sessions.lock().unwrap().active.values().map(|s|s.info()).collect::<Vec<_>>()})
            }
            ("GET", "/api/session/status") => {
                let current = h.current_app.lock().unwrap();
                let sessions = h.sessions.lock().unwrap();
                let uuid = current.as_ref().and_then(|running| {
                    h.apps
                        .read()
                        .unwrap()
                        .iter()
                        .find(|app| app.id() == running.id)
                        .and_then(|app| app.extra.get("uuid"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
                json!({"status":true,"activeSessions":sessions.active.len(),"appRunning":current.is_some(),"appName":current.as_ref().map(|a|a.name.as_str()).unwrap_or(""),"paused":current.is_some()&&sessions.active.is_empty(),"lastEncoderProbeFailed":false,"running":!sessions.active.is_empty(),"app":current.as_ref().map(|a|json!({"name":a.name,"id":a.id,"uuid":uuid}))})
            }
            ("GET", "/api/display-devices") => {
                serde_json::to_value(butterpollo_windows::display::monitors()?)?
            }
            ("GET", "/api/framegen/edid-refresh") => {
                let query: std::collections::HashMap<_, _> =
                    url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
                        .into_owned()
                        .collect();
                let hint = ["device_id", "device", "id", "display"]
                    .into_iter()
                    .find_map(|key| query.get(key).filter(|value| !value.trim().is_empty()))
                    .context("device_id query parameter is required")?;
                let mut targets = query
                    .get("targets")
                    .map(|text| {
                        text.split(',')
                            .filter_map(|value| value.trim().parse::<u32>().ok())
                            .filter(|hz| *hz > 0 && *hz <= 4000)
                            .take(64)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if targets.is_empty() {
                    targets = vec![120, 180, 240, 288];
                }
                butterpollo_windows::display::edid_refresh(hint, &targets)?
            }
            ("POST", "/api/otp") => {
                let passphrase = text("passphrase");
                if passphrase.chars().count() < 4 {
                    anyhow::bail!("passphrase must have at least four characters");
                }
                let pin = format!("{:04}", rand::Rng::gen_range(&mut rand::thread_rng(), 0..10_000u16));
                *h.otp.lock().unwrap() = Some(crate::state::OneTimePin {
                    pin: pin.clone(),
                    passphrase: passphrase.to_owned(),
                    device_name: text("deviceName").to_owned(),
                    created: Instant::now(),
                });
                let config = h.config.read().unwrap().clone();
                json!({
                    "status": true,
                    "otp": pin,
                    "ip": butterpollo_windows::net::lan_addresses().unwrap_or_default().first().cloned().unwrap_or_default(),
                    "name": crate::network::host_name(&config),
                    "message": "OTP created, effective within 3 minutes.",
                })
            }
            ("POST", "/api/pin") => {
                let pin = text("pin");
                if pin.len() != 4 || !pin.bytes().all(|c| c.is_ascii_digit()) {
                    anyhow::bail!("PIN must contain four digits");
                }
                let mut pins = h.pins.lock().unwrap();
                let key = if !text("uniqueid").is_empty() {
                    text("uniqueid").to_owned()
                } else {
                    if pins.len() != 1 {
                        anyhow::bail!("select one pending pairing request");
                    }
                    pins.keys()
                        .next()
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("no pending pairing request"))?
                };
                let p = pins
                    .remove(&key)
                    .ok_or_else(|| anyhow::anyhow!("pairing request no longer exists"))?;
                let name = if text("name").is_empty() {
                    p.name
                } else {
                    text("name").to_owned()
                };
                p.sender
                    .send((pin.into(), name))
                    .map_err(|_| anyhow::anyhow!("pairing request expired"))?;
                json!({"status":true})
            }
            ("POST", "/api/password") => {
                if let Some(c) = h.credentials.read().unwrap().as_ref()
                    && !c.verifies(text("currentUsername"), text("currentPassword"))
                {
                    anyhow::bail!("current credentials do not match");
                }
                let password = text("newPassword");
                if password != text("confirmNewPassword") {
                    anyhow::bail!("password confirmation does not match");
                }
                let c = Credentials::new(text("newUsername").into(), password)?;
                // Invalidate durable sessions before changing the password. A
                // failed credential write may require another login; it cannot
                // resurrect sessions authenticated by the previous password.
                let mut sessions = h.web_sessions.lock().unwrap();
                h.save_web_sessions(&Default::default())?;
                sessions.clear();
                h.save_credentials(&c)?;
                *h.credentials.write().unwrap() = Some(c);
                json!({"status":true})
            }
            ("POST", "/api/restart") => {
                h.restart.store(true, std::sync::atomic::Ordering::Release);
                h.stop.store(true, std::sync::atomic::Ordering::Release);
                json!({"status":true})
            }
            ("POST", "/api/quit") => {
                h.stop.store(true, std::sync::atomic::Ordering::Release);
                json!({"status":true})
            }
            ("DELETE", p) if p.starts_with("/api/apps/") => {
                delete_app(&h, &p[10..])?;
                json!({"status":true})
            }
            _ => {
                return Err(anyhow::anyhow!("unknown API endpoint"));
            }
        })
    }).await.unwrap_or_else(|e| Err(e.into()));
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}
fn delete_app(h: &Shared, id: &str) -> anyhow::Result<()> {
    let mut apps = h.apps.write().unwrap();
    let mut next = apps.clone();
    let before = next.len();
    next.retain(|a| {
        a.extra.get("uuid").and_then(Value::as_str) != Some(id) && a.id().to_string() != id
    });
    if next.len() == before {
        anyhow::bail!("application not found");
    }
    h.assign_apps(&mut next)?;
    let mut doc = h.app_document.read().unwrap().clone();
    doc["apps"] = serde_json::to_value(&next)?;
    state::write_json(&h.apps_path, &doc)?;
    *apps = next;
    *h.app_document.write().unwrap() = doc;
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn cover_requests_name_their_app_and_short_paths_do_not_panic() {
        assert_eq!(super::cover_id("/api/apps/42/cover"), Some("42"));
        assert_eq!(super::cover_id("/api/covers/7"), Some("7"));
        assert_eq!(super::cover_id("/api/apps/cover"), None);
        assert_eq!(super::cover_id("/api/apps//cover"), None);
        assert_eq!(super::cover_id("/api/covers/"), None);
    }
    #[test]
    fn sign_ins_are_limited_per_address_but_never_on_this_pc() {
        use std::net::IpAddr;
        let fresh = || super::SignIns {
            attempts: Default::default(),
        };
        let now = Instant::now();
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        let mut sign_ins = fresh();
        for _ in 0..10 {
            assert!(sign_ins.attempt(lan, now));
        }
        assert!(!sign_ins.attempt(lan, now), "the 11th attempt in a minute");
        assert!(sign_ins.attempt("192.168.1.21".parse().unwrap(), now));
        assert!(
            sign_ins.attempt(lan, now + Duration::from_secs(61)),
            "a minute later"
        );
        sign_ins.succeeded(lan);
        assert!(!sign_ins.attempts.contains_key(&lan));
        for local in ["127.0.0.1", "::1", "::ffff:127.0.0.1"] {
            let local: IpAddr = local.parse().unwrap();
            assert!((0..50).all(|_| sign_ins.attempt(local, now)), "{local}");
        }
        // One LAN host can take any address in its /64.
        let mut sign_ins = fresh();
        for host in 0..10 {
            assert!(sign_ins.attempt(format!("fe80::{host:x}").parse().unwrap(), now));
        }
        assert!(!sign_ins.attempt("fe80::abcd".parse().unwrap(), now));
        assert!(sign_ins.attempt("fd00:1:2:3::1".parse().unwrap(), now));
        // A full table forgets the oldest address instead of refusing new ones.
        let mut sign_ins = fresh();
        for host in 0..super::SignIns::TRACKED as u32 {
            let address = IpAddr::from(std::net::Ipv4Addr::from(0x0a00_0000 + host));
            assert!(sign_ins.attempt(address, now + Duration::from_millis(u64::from(host))));
        }
        let newcomer: IpAddr = "172.16.0.1".parse().unwrap();
        assert!(sign_ins.attempt(newcomer, now + Duration::from_secs(2)));
        assert_eq!(sign_ins.attempts.len(), super::SignIns::TRACKED);
        assert!(
            !sign_ins
                .attempts
                .contains_key(&"10.0.0.0".parse::<IpAddr>().unwrap())
        );
    }
    use super::*;

    #[test]
    fn web_app_files_stay_in_their_folder() {
        let base = std::env::temp_dir().join(format!("butterpollo-web-{}", uuid::Uuid::new_v4()));
        let root = base.join("web");
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("index.html"), "<html></html>").unwrap();
        std::fs::write(root.join("assets/index-1.js"), "export {}").unwrap();
        std::fs::write(base.join("outside.txt"), "x").unwrap();
        let get = |path: &str| app_file(&root, &Method::GET, path);
        let header = |response: &Response, name: header::HeaderName| {
            response
                .headers()
                .get(name)
                .map(|v| v.to_str().unwrap().to_owned())
        };

        let script = get("/assets/index-1.js");
        assert_eq!(script.status(), StatusCode::OK);
        assert!(
            header(&script, header::CACHE_CONTROL)
                .unwrap()
                .contains("immutable")
        );
        assert!(header(&script, header::CONTENT_SECURITY_POLICY).is_none());

        let page = get("/devices");
        assert_eq!(page.status(), StatusCode::OK);
        assert!(
            header(&page, header::CONTENT_TYPE)
                .unwrap()
                .starts_with("text/html")
        );
        assert!(header(&page, header::CACHE_CONTROL).is_none());
        assert_eq!(
            header(&page, header::CONTENT_SECURITY_POLICY).as_deref(),
            Some(APP_POLICY)
        );

        for escape in [
            "/../outside.txt",
            "/assets/../../outside.txt",
            "/C:/Windows/win.ini",
        ] {
            let response = get(escape);
            assert_ne!(
                header(&response, header::CONTENT_TYPE).as_deref(),
                Some("application/octet-stream"),
                "{escape}"
            );
        }
        assert_eq!(get("/assets/index-0.js").status(), StatusCode::NOT_FOUND);
        assert_eq!(
            app_file(&root, &Method::POST, "/devices").status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
