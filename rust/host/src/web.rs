mod apps;
mod clients;
mod config;
mod displays;
mod logs;
mod maintenance;
#[cfg(test)]
mod parity_tests;
mod session;

use crate::{state::Shared, tls::Connection};
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
use butterpollo_core::{auth, crypto, state, state::Credentials};
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
/// The app of an icon request: /api/apps/UUID/icon.
fn icon_id(path: &str) -> Option<&str> {
    path.strip_prefix("/api/apps/")
        .and_then(|rest| rest.strip_suffix("/icon"))
        .filter(|id| !id.is_empty() && !id.contains('/'))
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
        ("/api/configLocale", &["GET"][..]),
        ("/api/meta", &["GET"][..]),
        ("/api/metadata", &["GET"][..]),
        ("/api/apps", &["GET", "POST"][..]),
        ("/api/apps/[^/]+", &["DELETE"][..]),
        ("/api/apps/delete", &["POST"][..]),
        ("/api/apps/[^/]+/cover", &["GET"][..]),
        ("/api/apps/[^/]+/icon", &["GET"][..]),
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
        ("/api/playnite/cover", &["POST"][..]),
        ("/api/playnite/launch", &["POST"][..]),
        ("/api/apps/purge_autosync", &["POST"][..]),
        ("/api/lossless_scaling/status", &["GET"][..]),
        ("/api/browse", &["GET"][..]),
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
        return apps::upload_cover(&h, &data).await;
    }
    if let Some(action) = path.strip_prefix("/api/steam/") {
        return apps::steam(&h, &method, action, &uri, &data).await;
    }
    if let Some(action) = path.strip_prefix("/api/playnite/") {
        return apps::playnite(&h, &method, action, &data).await;
    }
    if method == Method::GET && path == "/api/lossless_scaling/status" {
        return apps::lossless_scaling(&h, &uri).await;
    }
    if method == Method::GET && path == "/api/browse" {
        return apps::browse(&uri).await;
    }
    if method == Method::GET
        && let Some(id) = icon_id(path)
    {
        return apps::icon(&h, id).await;
    }
    if method == Method::POST && path == "/api/apps/purge_autosync" {
        return apps::purge_autosync(&h);
    }
    if method == Method::GET && path == "/api/logs/export_crash" {
        return logs::export_crash(&h).await;
    }
    if method == Method::GET && path == "/api/logs/tail" {
        return logs::tail(&h, &uri);
    }
    if method == Method::GET && matches!(path, "/api/logs" | "/api/logs/export") {
        return logs::export(&h, path);
    }
    if method == Method::GET
        && ((path.starts_with("/api/apps/") && path.ends_with("/cover"))
            || path.starts_with("/api/covers/"))
    {
        return apps::cover(&h, path).await;
    }
    if path == "/api/auth/status" {
        let configured = h.credentials.read().unwrap().is_some();
        let authenticated = authenticated(&h, &headers, Some(connection.peer.ip()));
        // The web app asks this on load. A remembered browser whose access
        // cookie lapsed (after session_token_ttl_seconds) is renewed here, as
        // the console page does, instead of being sent to the login page.
        if configured
            && !authenticated
            && let Some(issued) = refresh_browser(&h, &headers, &connection)
            && issued.status().is_success()
        {
            let mut result = Json(json!({"authenticated":true,"credentials_configured":true,"login_required":false,"status":true})).into_response();
            for value in issued.headers().get_all(header::SET_COOKIE) {
                result
                    .headers_mut()
                    .append(header::SET_COOKIE, value.clone());
            }
            return result;
        }
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
            ("GET", "/api/updates")
            | ("POST", "/api/updates/install")
            | ("POST", "/api/updates/cancel")
            | ("POST", "/api/updates/check") => maintenance::handle(&h, method.as_str(), path)?,
            ("GET", "/api/health/crashdump")
            | ("POST", "/api/health/crashdump/dismiss")
            | ("GET", "/api/logs/export_crash/manifest") => {
                logs::handle(&h, method.as_str(), path, &data)?
            }
            ("GET", "/api/display/golden_status")
            | ("POST", "/api/display/export_golden")
            | ("POST", "/api/display/restore_golden")
            | ("POST", "/api/reset-display-device-persistence")
            | ("DELETE", "/api/display/golden")
            | ("POST", "/api/display/terminate_virtual")
            | ("GET", "/api/clients/display-layout")
            | ("PUT", "/api/clients/display-layout") => {
                displays::handle(&h, method.as_str(), &uri, &data)?
            }
            ("GET", "/api/rtss/status" | "/api/frame-limiter/status") => {
                session::handle(&h, method.as_str(), path)?
            }
            ("GET", "/api/health/vulkan-hdr-layer")
            | ("POST", "/api/health/vulkan-hdr-layer/register") => {
                displays::handle(&h, method.as_str(), &uri, &data)?
            }
            ("GET", "/api/auth/sessions") => {
                let sessions = h.web_sessions.lock().unwrap();
                let current = crate::web_sessions::resolve_hash(
                    &sessions,
                    &access(&headers).unwrap_or_default(),
                );
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
                            json!(current.as_ref().is_some_and(|current| crypto::equal(
                                token.as_bytes(),
                                current.as_bytes()
                            ))),
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
            ("GET", "/api/config")
            | ("POST" | "PATCH", "/api/config")
            | ("GET", "/api/configLocale")
            | ("GET", "/api/meta" | "/api/metadata") => {
                config::handle(&h, method.as_str(), path, &data)?
            }
            ("GET", "/api/apps")
            | ("POST", "/api/apps")
            | ("POST", "/api/apps/delete")
            | ("POST", "/api/apps/reorder")
            | ("POST", "/api/apps/rtx_hdr/live")
            | ("POST", "/api/apps/close")
            | ("POST", "/api/apps/launch") => apps::handle(&h, method.as_str(), path, &data)?,
            ("GET", "/api/clients/list") => clients::handle(&h, method.as_str(), path, &data)?,
            ("GET", "/api/clients/hdr-profiles") => {
                displays::handle(&h, method.as_str(), &uri, &data)?
            }
            ("POST", "/api/clients/unpair")
            | ("POST", "/api/clients/unpair-all")
            | ("POST", "/api/clients/disconnect")
            | ("POST", "/api/clients/update")
            | ("GET", "/api/clients/pending") => clients::handle(&h, method.as_str(), path, &data)?,
            ("GET", "/api/rtsp/sessions") | ("GET", "/api/session/status") => {
                session::handle(&h, method.as_str(), path)?
            }
            ("GET", "/api/display-devices") | ("GET", "/api/framegen/edid-refresh") => {
                displays::handle(&h, method.as_str(), &uri, &data)?
            }
            ("POST", "/api/otp") | ("POST", "/api/pin") => {
                clients::handle(&h, method.as_str(), path, &data)?
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
            ("POST", "/api/restart") | ("POST", "/api/quit") => {
                maintenance::handle(&h, method.as_str(), path)?
            }
            ("DELETE", p) if p.starts_with("/api/apps/") => {
                apps::handle(&h, method.as_str(), path, &data)?
            }
            _ => {
                return Err(anyhow::anyhow!("unknown API endpoint"));
            }
        })
    })
    .await
    .unwrap_or_else(|e| Err(e.into()));
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, &e.to_string()),
    }
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
        assert_eq!(super::icon_id("/api/apps/abc-1/icon"), Some("abc-1"));
        assert_eq!(super::icon_id("/api/apps/icon"), None);
        assert_eq!(super::icon_id("/api/apps//icon"), None);
        assert_eq!(super::icon_id("/api/apps/a/b/icon"), None);
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
