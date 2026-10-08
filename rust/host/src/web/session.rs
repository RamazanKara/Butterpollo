//! Streaming session and frame-limiter status API handlers.

use crate::state::Shared;
use serde_json::{Value, json};

pub(super) fn handle(h: &Shared, method: &str, path: &str) -> anyhow::Result<Value> {
    Ok(match (method, path) {
        ("GET", "/api/rtss/status" | "/api/frame-limiter/status") => {
            crate::maintenance::integration_status(h)
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
            json!({"status":true,"activeSessions":sessions.active.len(),"appRunning":current.is_some(),"appName":current.as_ref().map(|a|a.name.as_str()).unwrap_or(""),"paused":current.is_some()&&sessions.active.is_empty(),"lastEncoderProbeFailed":h.warnings.snapshot().iter().any(|warning| warning.code == "video_encoder"),"running":!sessions.active.is_empty(),"app":current.as_ref().map(|a|json!({"name":a.name,"id":a.id,"uuid":uuid}))})
        }
        _ => return Err(anyhow::anyhow!("unknown API endpoint")),
    })
}
