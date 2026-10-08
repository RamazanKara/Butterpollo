//! Application library, artwork, and launcher API handlers.

use super::{cover_id, error};
use crate::state::Shared;
use axum::{
    Json,
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use butterpollo_core::{state, state::App};
use serde_json::{Value, json};

pub(super) fn handle(h: &Shared, method: &str, path: &str, data: &Value) -> anyhow::Result<Value> {
    let text = |k: &str| data.get(k).and_then(Value::as_str).unwrap_or("");
    Ok(match (method, path) {
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
            delete_app(h, text("uuid"))?;
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
            if crate::updater::installing(h) {
                anyhow::bail!("Butterpollo is installing an update");
            }
            let uuid = text("uuid");
            let app = h
                .apps
                .read()
                .unwrap()
                .iter()
                .find(|a| a.extra.get("uuid").and_then(Value::as_str) == Some(uuid))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("application not found"))?;
            if h.current_app.lock().unwrap().is_some() {
                anyhow::bail!("application already running");
            }
            // Started without the lock every serverinfo request takes: its
            // prep commands can run for minutes. launch_transition keeps
            // another app from being installed meanwhile.
            let running = crate::process::launch(h, &app, &Default::default())?;
            let mut current = h.current_app.lock().unwrap();
            if current.is_some() {
                drop(current);
                drop(running);
                anyhow::bail!("application already running");
            }
            *current = Some(running);
            json!({"status":true})
        }
        ("DELETE", p) if p.starts_with("/api/apps/") => {
            delete_app(h, &p[10..])?;
            json!({"status":true})
        }
        _ => return Err(anyhow::anyhow!("unknown API endpoint")),
    })
}

pub(super) async fn upload_cover(h: &Shared, data: &Value) -> Response {
    let text = |k: &str| data.get(k).and_then(Value::as_str).unwrap_or("");
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
    match result {
        Ok(value) => Json(value).into_response(),
        Err(err) => error(StatusCode::BAD_REQUEST, &err.to_string()),
    }
}

pub(super) async fn steam(
    h: &Shared,
    method: &Method,
    action: &str,
    uri: &axum::http::Uri,
    data: &Value,
) -> Response {
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
    match task {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(err)) => error(StatusCode::BAD_REQUEST, &format!("{err:#}")),
        Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub(super) async fn playnite(h: &Shared, method: &Method, action: &str, data: &Value) -> Response {
    let text = |k: &str| data.get(k).and_then(Value::as_str).unwrap_or("");
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
        ("POST", "cover") => {
            let (id, key) = (text("playnite_id").to_owned(), text("cover_key").to_owned());
            if id.is_empty() || key.is_empty() {
                return error(StatusCode::BAD_REQUEST, "Playnite game ID and cover key are required");
            }
            tokio::task::spawn_blocking(move || {
                crate::playnite::set_cover(&h, &id, &key)
                    .map(|path| json!({"status":true,"path":path.to_string_lossy().replace('\\', "/")}))
            })
            .await
        }
        ("POST", "launch") => {
            tokio::task::spawn_blocking(|| crate::playnite::restart().map(|()| json!({"status":true}))).await
        }
        _ => return error(StatusCode::BAD_REQUEST, "unknown API endpoint"),
    };
    match task {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(err)) => error(StatusCode::BAD_REQUEST, &format!("{err:#}")),
        Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub(super) async fn lossless_scaling(h: &Shared, uri: &axum::http::Uri) -> Response {
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
    match result {
        Ok(value) => Json(value).into_response(),
        Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub(super) async fn browse(uri: &axum::http::Uri) -> Response {
    let query: std::collections::HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let filter = butterpollo_core::browse::Filter::parse(query.get("type").map(String::as_str));
    let requested = query.get("path").cloned().unwrap_or_default();
    // Listing a folder waits on the disk, or on the network for a share.
    let result = tokio::task::spawn_blocking(move || {
        if butterpollo_core::browse::root(&requested) {
            Ok(butterpollo_core::browse::drives(
                &butterpollo_windows::files::drives(),
            ))
        } else {
            butterpollo_core::browse::listing(std::path::Path::new(&requested), filter)
        }
    })
    .await;
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(message)) => error(StatusCode::BAD_REQUEST, &message),
        Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub(super) async fn icon(h: &Shared, id: &str) -> Response {
    let icon = h
        .apps
        .read()
        .unwrap()
        .iter()
        .find(|a| a.extra.get("uuid").and_then(Value::as_str) == Some(id))
        .and_then(butterpollo_core::catalog::icon);
    if let Some(icon) = icon
        && let Ok(bytes) = tokio::fs::read(icon).await
    {
        return ([(header::CONTENT_TYPE, "image/png")], bytes).into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}

pub(super) fn purge_autosync(h: &Shared) -> Response {
    let mut removed = 0;
    match crate::steam::update_apps(h, |apps| {
        removed = butterpollo_core::playnite::purge(apps);
        removed > 0
    }) {
        Ok(_) => Json(json!({"status":true,"removed":removed})).into_response(),
        Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{err:#}")),
    }
}

pub(super) async fn cover(h: &Shared, path: &str) -> Response {
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
    StatusCode::NOT_FOUND.into_response()
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
