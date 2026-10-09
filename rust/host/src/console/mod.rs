//! Administration is rendered and handled by Rust; the browser needs no script.
mod actions;
mod i18n;
mod pages;
mod settings;

use crate::{state::Shared, tls::Connection, web};
pub(crate) use actions::{action, route};
use axum::{
    Extension,
    body::{Bytes, to_bytes},
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{Html, IntoResponse, Response},
};
use butterpollo_core::crypto;
use serde_json::Value;
use std::{collections::BTreeMap, time::Instant};
pub(crate) type Fields = BTreeMap<String, String>;
const NAV: [(&str, &str); 8] = [
    ("/", "Overview"),
    ("/library", "Library"),
    ("/devices", "Devices"),
    ("/logs", "Logs"),
    ("/settings", "Settings"),
    ("/integrations", "Integrations"),
    ("/api-tokens", "API tokens"),
    ("/maintenance", "Maintenance"),
];
pub(crate) const PERMISSIONS: [(u32, &str); 13] = [
    (0x01000000, "List apps"),
    (0x02000000, "View streams"),
    (0x04000000, "Launch apps"),
    (0x00000100, "Controllers"),
    (0x00000200, "Touch"),
    (0x00000400, "Pen"),
    (0x00000800, "Mouse"),
    (0x00001000, "Keyboard"),
    (0x00010000, "Write to the clipboard"),
    (0x00020000, "Read the clipboard"),
    (0x00040000, "Upload files"),
    (0x00080000, "Download files"),
    (0x00100000, "Host commands"),
];
pub(crate) fn esc(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            '\u{fdd0}' => output.push_str("&#64976;"),
            '\u{fdd1}' => output.push_str("&#64977;"),
            '\u{fdd2}' => output.push_str("&#64978;"),
            '\u{fdd3}' => output.push_str("&#64979;"),
            _ => output.push(c),
        }
    }
    output
}
pub(crate) fn encoded(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
pub(crate) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
pub(crate) fn field(name: &str, label: &str, value: &str, kind: &str) -> String {
    format!(
        "<label>{}<input name=\"{}\" type=\"{}\" value=\"{}\" autocomplete=\"{}\"></label>",
        i18n::label(name, label),
        esc(name),
        esc(kind),
        i18n::data(value),
        if kind == "password" {
            if name.starts_with("current") || name == "password" {
                "current-password"
            } else {
                "new-password"
            }
        } else {
            "off"
        }
    )
}
pub(crate) fn select(name: &str, label: &str, value: &str, options: &[(&str, &str)]) -> String {
    let mut html = format!(
        "<label>{}<select name=\"{}\" aria-label=\"{}\">",
        i18n::label(name, label),
        esc(name),
        i18n::label(name, label)
    );
    if !options.iter().any(|o| o.0 == value) {
        html += &format!(
            "<option selected value=\"{}\">{}</option>",
            esc(value),
            i18n::data(value)
        );
    }
    for &(key, label) in options {
        html += &format!(
            "<option value=\"{}\"{}>{}</option>",
            esc(key),
            if key == value { " selected" } else { "" },
            i18n::message(label)
        );
    }
    html + "</select></label>"
}
pub(crate) fn area(name: &str, label: &str, value: &str) -> String {
    format!(
        "<label>{}<textarea name=\"{}\" aria-label=\"{}\" rows=\"8\" spellcheck=\"false\">{}</textarea></label>",
        i18n::label(name, label),
        esc(name),
        i18n::label(name, label),
        i18n::data(value)
    )
}
pub(crate) fn form(op: &str, csrf: &str, back: &str, content: &str) -> String {
    format!(
        "<form method=\"post\" action=\"/console/action\"><input type=\"hidden\" name=\"op\" value=\"{}\"><input type=\"hidden\" name=\"_csrf\" value=\"{}\"><input type=\"hidden\" name=\"_return\" value=\"{}\">{}</form>",
        esc(op),
        esc(csrf),
        esc(back),
        content
    )
}
pub(crate) fn hidden(name: &str, value: &str) -> String {
    format!(
        "<input type=\"hidden\" name=\"{}\" value=\"{}\">",
        esc(name),
        esc(value)
    )
}
pub(crate) fn button(
    op: &str,
    csrf: &str,
    back: &str,
    key: &str,
    value: &str,
    label: &str,
) -> String {
    form(
        op,
        csrf,
        back,
        &(hidden(key, value)
            + &format!(
                "<button class=\"secondary\">{}</button>",
                i18n::message(label)
            )),
    )
}
fn page_path(path: &str) -> Option<&str> {
    Some(match path {
        "/overview" => "/",
        "/apps" => "/library",
        "/clients" | "/pin" => "/devices",
        "/config" => "/settings",
        p if NAV.iter().any(|n| n.0 == p) || matches!(p, "/login" | "/setup") => p,
        _ => return None,
    })
}
pub(crate) fn return_path(value: &str) -> &str {
    page_path(value).unwrap_or("/")
}
fn redirect(path: &str) -> Response {
    (StatusCode::SEE_OTHER, [(header::LOCATION, path)]).into_response()
}
fn html(body: String) -> Response {
    let mut response = Html(body).into_response();
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; style-src 'self'; img-src 'self' data:; form-action 'self'; base-uri 'none'; frame-ancestors 'none'".parse().unwrap());
    response
}
pub(crate) async fn get(
    h: &Shared,
    connection: &Connection,
    headers: &HeaderMap,
    path: &str,
) -> Result<Value, String> {
    let response = web::api(
        State(h.clone()),
        Extension(connection.clone()),
        Method::GET,
        path.parse().unwrap(),
        headers.clone(),
        Bytes::new(),
    )
    .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(text(&value, "error").to_owned());
    }
    Ok(value)
}
fn csrf(h: &Shared, headers: &HeaderMap) -> (String, bool) {
    if let Some(token) = web::access(headers)
        && let Some(session) = h
            .web_sessions
            .lock()
            .unwrap()
            .get(&crate::web_sessions::hash(&token))
        && session.expires > Instant::now()
    {
        return (session.csrf.clone(), false);
    }
    if let Some(token) = web::cookie(headers, "__Host-apollo_anon_csrf")
        && token.len() == 64
        && token.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return (token, false);
    }
    (hex::encode(crypto::random::<32>()), true)
}
pub(crate) fn shell(
    title: &str,
    path: &str,
    headers: &HeaderMap,
    csrf: &str,
    body: &str,
    signed_in: bool,
) -> String {
    let theme = web::cookie(headers, "butterpollo_theme")
        .filter(|t| matches!(t.as_str(), "light" | "dark" | "system"))
        .unwrap_or_else(|| "system".into());
    let mut html = format!(
        "<!doctype html><html lang=\"en\" data-theme=\"{}\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{} · Rubylight</title><link rel=\"stylesheet\" href=\"/console.css\"><link rel=\"icon\" href=\"/favicon.svg\"></head><body>",
        esc(&theme),
        i18n::message(title)
    );
    if signed_in {
        html += "<aside><a class=\"brand\" href=\"/\"><span class=\"mark\">R</span>Rubylight</a><nav aria-label=\"Main navigation\">";
        for &(url, label) in &NAV {
            html += &format!(
                "<a href=\"{url}\"{}>{label}</a>",
                if url == path {
                    " aria-current=\"page\""
                } else {
                    ""
                }
            );
        }
        html += "</nav><div class=\"sidebar-bottom\">";
        html += &form(
            "theme",
            csrf,
            path,
            &(select(
                "theme",
                "Appearance",
                &theme,
                &[("system", "System"), ("light", "Light"), ("dark", "Dark")],
            ) + "<button class=\"secondary\">Apply</button>"),
        );
        html += &button("logout", csrf, "/login", "", "", "Sign out");
        html += "<small>Rubylight</small></div></aside><main>";
    } else {
        html += "<main class=\"auth\"><a class=\"brand\" href=\"/\"><span class=\"mark\">R</span>Rubylight</a>";
    }
    html += &format!(
        "<header><p class=\"eyebrow\">Your streaming host</p><h1>{}</h1></header>{body}</main></body></html>",
        i18n::message(title)
    );
    html
}
pub(crate) async fn page(
    State(h): State<Shared>,
    Extension(connection): Extension<Connection>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if !matches!(method, Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let Some(path) = page_path(uri.path()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let configured = h.credentials.read().unwrap().is_some();
    let signed_in = web::authenticated(&h, &headers, Some(connection.peer.ip()));
    if !configured && path != "/setup" {
        return redirect("/setup");
    }
    if configured
        && !signed_in
        && method == Method::GET
        && let Some(issued) = web::refresh_browser(&h, &headers, &connection)
    {
        if !issued.status().is_success() {
            return issued;
        }
        let mut result = redirect(uri.path_and_query().map_or("/", |p| p.as_str()));
        for value in issued.headers().get_all(header::SET_COOKIE) {
            result
                .headers_mut()
                .append(header::SET_COOKIE, value.clone());
        }
        return result;
    }
    if configured && !signed_in && path != "/login" {
        return redirect("/login");
    }
    if configured && signed_in && matches!(path, "/login" | "/setup") {
        return redirect("/");
    }
    let (csrf, set_cookie) = csrf(&h, &headers);
    let query: Fields = url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
        .into_owned()
        .collect();
    let title = NAV.iter().find(|n| n.0 == path).map_or(
        if path == "/setup" {
            "Set up this host"
        } else {
            "Sign in"
        },
        |n| n.1,
    );
    let mut body = String::new();
    if let Some(notice) = query.get("notice") {
        body += &format!(
            "<div class=\"notice\" role=\"status\">{}</div>",
            i18n::data(notice)
        );
    }
    match pages::render(&h, &connection, &headers, path, &csrf, &query).await {
        Ok(content) => body += &content,
        Err(error) => {
            body += &format!(
                "<div class=\"notice error\" role=\"alert\">{}</div>",
                i18n::data(&error)
            )
        }
    }
    let locale = h.config.read().unwrap().get("locale", "en").to_owned();
    let body = shell(title, path, &headers, &csrf, &body, signed_in).replacen(
        "lang=\"en\"",
        &format!("lang=\"{}\"", i18n::locale(&locale).replace('_', "-")),
        1,
    );
    let mut response = html(i18n::render(&body, &locale));
    if path == "/" && query.get("live").is_some_and(|v| v == "1") {
        response
            .headers_mut()
            .insert("refresh", "5; url=/?live=1".parse().unwrap());
    }
    if set_cookie {
        response.headers_mut().append(header::SET_COOKIE, format!("__Host-apollo_anon_csrf={csrf}; Path=/; HttpOnly; SameSite=Strict; Secure; Max-Age=3600").parse().unwrap());
    }
    response
}
pub(crate) async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("style.css"),
    )
}
pub(crate) async fn favicon() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/svg+xml")],
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 32 32\"><rect width=\"32\" height=\"32\" rx=\"8\" fill=\"#f2b705\"/><text x=\"16\" y=\"24\" font-family=\"sans-serif\" font-size=\"24\" font-weight=\"700\" text-anchor=\"middle\" fill=\"#16181c\">R</text></svg>",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_and_attribute_escaping() {
        assert_eq!(esc("<script>\"&'"), "&lt;script&gt;&quot;&amp;&#39;");
    }
    #[test]
    fn redirects_are_local_and_allowlisted() {
        for path in [
            "https://evil.test",
            "//evil.test",
            "/settings?next=https://evil.test",
            "/api/config",
        ] {
            assert_eq!(return_path(path), "/");
        }
    }
}
