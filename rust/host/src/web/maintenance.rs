//! Host update and lifecycle API handlers.

use crate::state::Shared;
use serde_json::{Value, json};

pub(super) fn handle(h: &Shared, method: &str, path: &str) -> anyhow::Result<Value> {
    Ok(match (method, path) {
        ("GET", "/api/updates") => crate::updater::status(h),
        ("POST", "/api/updates/install") => {
            crate::updater::queue(h, false)?;
            json!({"status":true})
        }
        ("POST", "/api/updates/cancel") => {
            crate::updater::cancel(h)?;
            json!({"status":true})
        }
        ("POST", "/api/updates/check") => {
            crate::maintenance::trigger_update(h, false);
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
        _ => return Err(anyhow::anyhow!("unknown API endpoint")),
    })
}
