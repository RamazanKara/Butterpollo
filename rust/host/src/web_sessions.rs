//! Previous root.session_tokens format; only one-way token hashes are persisted.
use butterpollo_core::crypto;
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[derive(Clone)]
pub struct WebSession {
    pub created: u64,
    pub last_seen: u64,
    pub refresh: String,
    pub csrf: String,
    pub expires: Instant,
    pub refresh_expires: Instant,
    pub access_deadline: u64,
    pub refresh_deadline: u64,
    pub username: String,
    pub remember_me: bool,
    pub user_agent: String,
    pub remote_address: String,
    pub extra: Map<String, Value>,
}
#[derive(Debug)]
pub struct Rotated;
impl std::fmt::Display for Rotated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("refresh token already rotated or expired")
    }
}
impl std::error::Error for Rotated {}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn hash(secret: &str) -> String {
    hex::encode(crypto::hash(secret.as_bytes()))
}
pub fn resolve_hash(sessions: &HashMap<String, WebSession>, secret: &str) -> Option<String> {
    if secret.is_empty() {
        return None;
    }
    let current = hash(secret);
    if sessions.contains_key(&current) {
        return Some(current);
    }
    let previous = crypto::legacy_hash(secret.as_bytes()).to_ascii_lowercase();
    sessions.contains_key(&previous).then_some(previous)
}
pub fn find<'a>(sessions: &'a HashMap<String, WebSession>, secret: &str) -> Option<&'a WebSession> {
    sessions.get(&resolve_hash(sessions, secret)?)
}
pub fn device_label(user_agent: &str, remote_address: &str) -> String {
    if user_agent.is_empty() {
        return if remote_address.is_empty() {
            "Unknown device".into()
        } else {
            remote_address.chars().take(128).collect()
        };
    }
    let ua = user_agent.to_ascii_lowercase();
    let browser = [
        ("edg/", "Edge"),
        ("opr/", "Opera"),
        ("firefox/", "Firefox"),
        ("chrome/", "Chrome"),
        ("safari/", "Safari"),
    ]
    .iter()
    .find(|(part, _)| ua.contains(part))
    .map(|(_, name)| *name);
    let os = [
        ("android", "Android"),
        ("iphone", "iOS"),
        ("ipad", "iOS"),
        ("windows", "Windows"),
        ("macintosh", "macOS"),
        ("linux", "Linux"),
    ]
    .iter()
    .find(|(part, _)| ua.contains(part))
    .map(|(_, name)| *name);
    match (browser, os) {
        (Some(browser), Some(os)) => format!("{browser} on {os}"),
        (Some(name), None) | (None, Some(name)) => name.into(),
        _ => user_agent.chars().take(128).collect(),
    }
}
fn number(value: &Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .unwrap_or(0)
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}
pub fn load(document: &Value, username: &str) -> HashMap<String, WebSession> {
    let mut sessions = HashMap::new();
    let wall = now();
    let tick = Instant::now();
    for row in document["root"]["session_tokens"]
        .as_array()
        .into_iter()
        .flatten()
        .take(256)
    {
        let text = |key| row[key].as_str().unwrap_or("");
        let key = text("hash");
        let refresh = text("refresh_token_hash");
        let access_deadline = number(&row["expires_at"]);
        let saved_refresh = number(&row["refresh_expires_at"]);
        let refresh_deadline = if valid_hash(refresh) && saved_refresh != 0 {
            saved_refresh
        } else {
            access_deadline
        };
        if !valid_hash(key)
            || !text("username").eq_ignore_ascii_case(username)
            || username.is_empty()
            || refresh_deadline <= wall
            || refresh_deadline - wall > 366 * 86400
            || sessions.len() >= 64
        {
            continue;
        }
        sessions.insert(
            key.to_ascii_lowercase(),
            WebSession {
                created: number(&row["created_at"]),
                last_seen: number(&row["last_seen"]),
                refresh: refresh.to_ascii_lowercase(),
                csrf: hex::encode(crypto::random::<32>()),
                expires: tick
                    + Duration::from_secs(
                        access_deadline.min(refresh_deadline).saturating_sub(wall),
                    ),
                refresh_expires: tick + Duration::from_secs(refresh_deadline - wall),
                access_deadline: access_deadline.min(refresh_deadline),
                refresh_deadline,
                username: username.into(),
                remember_me: row["remember_me"]
                    .as_bool()
                    .unwrap_or_else(|| matches!(text("remember_me"), "true" | "1")),
                user_agent: text("user_agent").chars().take(512).collect(),
                remote_address: text("remote_address").chars().take(128).collect(),
                extra: row.as_object().cloned().unwrap_or_default(),
            },
        );
    }
    sessions
}
pub fn record(key: &str, session: &WebSession) -> Value {
    let mut row = session.extra.clone();
    for (name, value) in json!({"hash":key,"username":session.username,"created_at":session.created,
        "expires_at":session.access_deadline,"refresh_expires_at":session.refresh_deadline,
        "refresh_token_hash":session.refresh,"last_seen":session.last_seen,"remember_me":session.remember_me,
        "user_agent":session.user_agent,"remote_address":session.remote_address}).as_object().unwrap() {
        row.insert(name.clone(), value.clone());
    }
    if row
        .get("device_label")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        row.insert(
            "device_label".into(),
            device_label(&session.user_agent, &session.remote_address).into(),
        );
    }
    // Legacy imports must never re-persist an accidentally supplied raw secret.
    for key in [
        "access_token",
        "session_token",
        "refresh_token",
        "csrf_token",
    ] {
        row.remove(key);
    }
    Value::Object(row)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn previous_sessions_survive_restart_without_persisting_secrets() {
        let access = "old-browser-access";
        let refresh = "old-browser-refresh";
        let expires = now() + 100;
        for previous in [false, true] {
            let encode = |secret: &str| {
                if previous {
                    crypto::legacy_hash(secret.as_bytes())
                } else {
                    hash(secret).to_uppercase()
                }
            };
            let doc = json!({"root":{"session_tokens":[{"hash":encode(access),"username":"test",
            "created_at":"17","expires_at":expires.to_string(),"refresh_expires_at":(expires+1000).to_string(),
            "refresh_token_hash":encode(refresh),"remember_me":"true","custom-field":9,"refresh_token":refresh}]}});
            let loaded = load(&doc, "test");
            let key = resolve_hash(&loaded, access).unwrap();
            let s = find(&loaded, access).unwrap();
            assert!(crypto::matches_hash(refresh.as_bytes(), &s.refresh));
            assert!(find(&loaded, "forged").is_none());
            assert!(find(&loaded, "").is_none());
            assert!(s.expires > Instant::now());
            assert!(s.remember_me);
            assert_eq!(s.created, 17);
            let row = record(&key, s);
            assert_eq!(row["custom-field"], 9);
            assert!(row.get("refresh_token").is_none());
            let restored = load(&json!({"root":{"session_tokens":[row]}}), "test");
            assert_eq!(
                find(&restored, access).unwrap().refresh_deadline,
                expires + 1000
            );
            assert!(load(&doc, "other").is_empty());
        }
    }
}
