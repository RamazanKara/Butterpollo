use super::*;
use axum::extract::Form;
use serde_json::{Map, json};

/// The guard authorizes the exact API operation behind a form, including CSRF
/// and token scopes. No form field can supply an arbitrary path or HTTP method.
pub(crate) fn route(fields: &Fields) -> Result<(Method, String), String> {
    let op = fields.get("op").map(String::as_str).unwrap_or("");
    let (method, path) = match op {
        "login" => (Method::POST, "/api/auth/login"),
        "setup" | "password" => (Method::POST, "/api/password"),
        "logout" => (Method::POST, "/api/auth/logout"),
        "config" | "theme" => (Method::POST, "/api/config"),
        "app-save" => (Method::POST, "/api/apps"),
        "app-move" => (Method::POST, "/api/apps/reorder"),
        "app-live" => (Method::POST, "/api/apps/rtx_hdr/live"),
        "app-delete" => {
            let uuid = fields
                .get("uuid")
                .ok_or("application identifier required")?;
            if uuid.is_empty()
                || uuid.len() > 64
                || !uuid.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
            {
                return Err("invalid application identifier".into());
            }
            return Ok((Method::DELETE, format!("/api/apps/{uuid}")));
        }
        "app-launch" => (Method::POST, "/api/apps/launch"),
        "app-close" => (Method::POST, "/api/apps/close"),
        "client-save" => (Method::POST, "/api/clients/update"),
        "unpair" => (Method::POST, "/api/clients/unpair"),
        "disconnect" => (Method::POST, "/api/clients/disconnect"),
        "pair" => (Method::POST, "/api/pin"),
        "layout" => (Method::PUT, "/api/clients/display-layout"),
        "token-create" => (Method::POST, "/api/token"),
        "token-delete" | "session-revoke" => {
            let id = fields.get("id").ok_or("identifier required")?;
            if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("invalid identifier".into());
            }
            return Ok((
                Method::DELETE,
                format!(
                    "{}{}",
                    if op == "token-delete" {
                        "/api/token/"
                    } else {
                        "/api/auth/sessions/"
                    },
                    id
                ),
            ));
        }
        "golden-capture" => (Method::POST, "/api/display/export_golden"),
        "golden-restore" => (Method::POST, "/api/display/restore_golden"),
        "golden-delete" => (Method::DELETE, "/api/display/golden"),
        "vdd-terminate" => (Method::POST, "/api/display/terminate_virtual"),
        "crash-dismiss" => (Method::POST, "/api/health/crashdump/dismiss"),
        "check-update" => (Method::POST, "/api/updates/check"),
        "restart" => (Method::POST, "/api/restart"),
        _ => return Err("unknown console action".into()),
    };
    Ok((method, path.to_owned()))
}
fn object(fields: &Fields, key: &str) -> Result<Map<String, Value>, String> {
    let value = fields
        .get(key)
        .map(String::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("{}");
    serde_json::from_str::<Value>(value)
        .map_err(|e| format!("{key}: {e}"))?
        .as_object()
        .cloned()
        .ok_or_else(|| format!("{key} must be a JSON object"))
}
fn payload(h: &Shared, fields: &Fields) -> Result<Value, String> {
    let value = |key: &str| fields.get(key).map(String::as_str).unwrap_or("");
    let copy = |keys: &[&str]| {
        keys.iter()
            .map(|&k| (k.to_owned(), json!(value(k))))
            .collect::<Map<String, Value>>()
    };
    Ok(match value("op") {
        "login" => Value::Object(copy(&["username", "password", "remember_me"])),
        "setup" | "password" => Value::Object(copy(&[
            "currentUsername",
            "currentPassword",
            "newUsername",
            "newPassword",
            "confirmNewPassword",
        ])),
        "config" => {
            let mut config = object(fields, "advanced")?;
            super::settings::apply("cfg_", super::settings::GLOBAL, fields, &mut config)?;
            for key in [
                "sunshine_name",
                "encoder",
                "capture",
                "output_name",
                "virtual_display_mode",
                "audio_sink",
            ] {
                if let Some(value) = fields.get(key) {
                    config.insert(key.into(), json!(value));
                }
            }
            Value::Object(config)
        }
        "app-save" => {
            let mut app = object(fields, "advanced")?;
            // Merge unknown fields from the durable record, even when a normal
            // form is submitted without its optional advanced editor.
            let existing = h
                .apps
                .read()
                .unwrap()
                .iter()
                .find(|a| a.extra.get("uuid").and_then(Value::as_str) == Some(value("uuid")))
                .cloned();
            if let Some(existing) = existing {
                let mut original = serde_json::to_value(existing)
                    .map_err(|e| e.to_string())?
                    .as_object()
                    .cloned()
                    .unwrap();
                original.extend(app);
                app = original;
            }
            app.extend(copy(&["name", "cmd", "working-dir"]));
            super::settings::apply("app_", super::settings::APP, fields, &mut app)?;
            if let Some(overrides) = app
                .get_mut("config-overrides")
                .and_then(Value::as_object_mut)
            {
                for key in crate::stream::RTX_KEYS {
                    if fields.contains_key(&format!("app_{}", key.replace('_', "-"))) {
                        overrides.remove(*key);
                    }
                }
            }
            if value("uuid").is_empty() {
                app.remove("uuid");
            } else {
                app.insert("uuid".into(), json!(value("uuid")));
            }
            Value::Object(app)
        }
        "app-move" => {
            let apps = h.apps.read().unwrap();
            let mut order: Vec<_> = apps
                .iter()
                .map(|app| app.extra.get("uuid").cloned().unwrap_or(Value::Null))
                .collect();
            let index = order
                .iter()
                .position(|uuid| uuid.as_str() == Some(value("uuid")))
                .ok_or("application not found")?;
            match value("direction") {
                "up" if index > 0 => order.swap(index, index - 1),
                "down" if index + 1 < order.len() => order.swap(index, index + 1),
                "up" | "down" => {}
                _ => return Err("invalid ordering direction".into()),
            }
            json!({"order": order})
        }
        "app-live" => json!({"uuid":value("uuid"),"config-overrides":object(fields,"overrides")?}),
        "client-save" => {
            let mut client = object(fields, "advanced")?;
            client.remove("cert");
            client.extend(copy(&["uuid", "name"]));
            super::settings::apply("client_", super::settings::CLIENT, fields, &mut client)?;
            let perm = PERMISSIONS.iter().fold(0, |mask, (bit, _)| {
                if fields.contains_key(&format!("perm_{bit}")) {
                    mask | bit
                } else {
                    mask
                }
            });
            client.insert("perm".into(), json!(perm));
            client.insert("enabled".into(), json!(fields.contains_key("enabled")));
            Value::Object(client)
        }
        "pair" => Value::Object(copy(&["pin", "name", "uniqueid"])),
        "app-delete" | "app-launch" | "unpair" | "disconnect" => Value::Object(copy(&["uuid"])),
        "layout" => {
            serde_json::from_str(value("layout")).map_err(|e| format!("Display layout: {e}"))?
        }
        "token-create" => {
            json!({"scopes":serde_json::from_str::<Value>(value("scopes")).map_err(|e| format!("Token scopes: {e}"))?})
        }
        "crash-dismiss" => {
            json!({"filename":value("filename"),"captured_at":value("captured_at")})
        }
        _ => json!({}),
    })
}
fn notice(path: &str, message: &str) -> Response {
    (
        StatusCode::SEE_OTHER,
        [(
            header::LOCATION,
            format!("{}?notice={}", return_path(path), encoded(message)),
        )],
    )
        .into_response()
}
pub(crate) async fn action(
    State(h): State<Shared>,
    Extension(connection): Extension<Connection>,
    headers: HeaderMap,
    Form(fields): Form<Fields>,
) -> Response {
    let back = return_path(fields.get("_return").map(String::as_str).unwrap_or("/"));
    let op = fields.get("op").map(String::as_str).unwrap_or("");
    let (method, path) = match route(&fields) {
        Ok(route) => route,
        Err(error) => return notice(back, &error),
    };
    if op == "theme" {
        let theme = fields.get("theme").map(String::as_str).unwrap_or("system");
        if !matches!(theme, "light" | "dark" | "system") {
            return notice(back, "Invalid appearance");
        }
        let mut response = super::redirect(back);
        response.headers_mut().append(header::SET_COOKIE, format!("butterpollo_theme={theme}; Path=/; HttpOnly; SameSite=Strict; Secure; Max-Age=31536000").parse().unwrap());
        return response;
    }
    let data = match payload(&h, &fields) {
        Ok(data) => data,
        Err(error) => return notice(back, &error),
    };
    let response = web::api(
        State(h.clone()),
        Extension(connection),
        method,
        path.parse().unwrap(),
        headers.clone(),
        Bytes::from(data.to_string()),
    )
    .await;
    let status = response.status();
    let cookies: Vec<_> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .cloned()
        .collect();
    let bytes = match to_bytes(response.into_body(), 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => return notice(back, &error.to_string()),
    };
    let result: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let mut response = if !status.is_success() || result.get("status") == Some(&Value::Bool(false))
    {
        notice(back, text(&result, "error"))
    } else if op == "token-create" {
        // Secrets are displayed once in a no-store response, never in a URL.
        let csrf = super::csrf(&h, &headers).0;
        let body = format!(
            "<section class=\"card\"><h2>Copy your API token</h2><p>It will not be shown again.</p><pre class=\"secret\">{}</pre><a class=\"button\" href=\"/api-tokens\">Done</a></section>",
            esc(text(&result, "token"))
        );
        super::html(shell(
            "API tokens",
            "/api-tokens",
            &headers,
            &csrf,
            &body,
            true,
        ))
    } else {
        match op {
            "login" => super::redirect("/"),
            "logout" => super::redirect("/login"),
            "setup" | "password" => notice("/login", "Credentials saved. Sign in to continue."),
            "config" => notice(back, "Settings saved. Restart the host to apply them."),
            "restart" => notice(back, "Host is restarting. Reload this page in a moment."),
            _ => notice(back, "Saved successfully."),
        }
    };
    for cookie in cookies {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn form_actions_cannot_inject_paths_or_methods() {
        assert!(route(&Fields::from([("op".into(), "POST /api/quit".into())])).is_err());
        assert!(
            route(&Fields::from([
                ("op".into(), "token-delete".into()),
                ("id".into(), "../../config".into())
            ]))
            .is_err()
        );
        assert_eq!(
            route(&Fields::from([("op".into(), "theme".into())])).unwrap(),
            (Method::POST, "/api/config".into())
        );
    }
}
