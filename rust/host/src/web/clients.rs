//! Paired-client management and pairing API handlers.

use crate::state::Shared;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub(super) fn handle(h: &Shared, method: &str, path: &str, data: &Value) -> anyhow::Result<Value> {
    let text = |k: &str| data.get(k).and_then(Value::as_str).unwrap_or("");
    Ok(match (method, path) {
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
        ("POST", "/api/clients/unpair") => {
            h.sessions.lock().unwrap().request_stop(Some(text("uuid")));
            crate::remote_display::disconnect(h, Some(text("uuid")));
            h.paired
                .write()
                .unwrap()
                .remove(&h.paired_path, text("uuid"))?;
            json!({"status":true})
        }
        ("POST", "/api/clients/unpair-all") => {
            h.sessions.lock().unwrap().request_stop(None);
            crate::remote_display::disconnect(h, None);
            let mut state = h.paired.write().unwrap();
            let mut next = state.clone();
            next.clients.clear();
            next.save(&h.paired_path)?;
            *state = next;
            json!({"status":true})
        }
        ("POST", "/api/clients/disconnect") => {
            h.sessions.lock().unwrap().request_stop(Some(text("uuid")));
            crate::remote_display::disconnect(h, Some(text("uuid")));
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
                crate::remote_display::disconnect(h, Some(text("uuid")));
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
        ("POST", "/api/otp") => {
            let passphrase = text("passphrase");
            if passphrase.chars().count() < 4 {
                anyhow::bail!("passphrase must have at least four characters");
            }
            let pin = format!(
                "{:04}",
                rand::Rng::gen_range(&mut rand::thread_rng(), 0..10_000u16)
            );
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
        _ => return Err(anyhow::anyhow!("unknown API endpoint")),
    })
}
