use crate::crypto;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs::OpenOptions,
    io::Write,
    path::Path,
};

/// Never turn an unreadable existing identity file into a fresh empty host.
pub fn load_json(path: &Path, default: Value) -> Result<Value> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b)
            .with_context(|| format!("invalid JSON in {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default),
        Err(e) => Err(e.into()),
    }
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("state path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| -> Result<()> {
        let mut f = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            unsafe extern "system" {
                fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
            }
            let a: Vec<_> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
            let b: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 0x1 | 0x8) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        #[cfg(not(windows))]
        {
            std::fs::rename(&tmp, path)?;
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?)
}
fn default_permission() -> u32 {
    0x071f1f00
}
fn enabled() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Client {
    pub name: String,
    pub cert: String,
    pub uuid: String,
    #[serde(default = "default_permission", deserialize_with = "permission_number")]
    pub perm: u32,
    #[serde(default = "enabled", deserialize_with = "compatible_bool")]
    pub enabled: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
fn permission_number<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<u32, D::Error> {
    let v = Value::deserialize(d)?;
    match v {
        Value::String(s) => s.parse().map_err(serde::de::Error::custom),
        Value::Number(n) => n
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| serde::de::Error::custom("invalid permissions")),
        _ => Err(serde::de::Error::custom("invalid permissions")),
    }
}
fn compatible_bool<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    match Value::deserialize(d)? {
        Value::Bool(b) => Ok(b),
        Value::String(s) if matches!(s.as_str(), "true" | "1") => Ok(true),
        Value::String(s) if matches!(s.as_str(), "false" | "0") => Ok(false),
        Value::Number(n) if n.as_u64() == Some(0) => Ok(false),
        Value::Number(n) if n.as_u64() == Some(1) => Ok(true),
        _ => Err(serde::de::Error::custom("invalid boolean")),
    }
}
impl Client {
    pub fn allows(&self, permission: u32) -> bool {
        self.enabled && self.perm & permission == permission
    }
    pub fn der(&self) -> Result<Vec<u8>> {
        crypto::certificate_der(&self.cert)
    }
}
#[derive(Clone)]
pub struct PairedState {
    pub document: Value,
    pub clients: Vec<Client>,
    pub unique_id: String,
}
impl PairedState {
    pub fn load(path: &Path) -> Result<Self> {
        let mut document = load_json(
            path,
            json!({"root":{"uniqueid":uuid::Uuid::new_v4().to_string(),"named_devices":[]}}),
        )?;
        if !document.is_object() {
            bail!("paired state must be an object");
        }
        if document.get("root").is_none() {
            document["root"] = json!({});
        }
        let root = &mut document["root"];
        if !root.is_object() {
            bail!("paired state root must be an object");
        }
        let unique_id = match root.get("uniqueid") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(_) => bail!("invalid persisted host identity"),
            None => {
                let s = uuid::Uuid::new_v4().to_string();
                root["uniqueid"] = Value::String(s.clone());
                s
            }
        };
        let mut clients: Vec<Client> = match root.get("named_devices") {
            None => vec![],
            Some(v) => {
                serde_json::from_value(v.clone()).context("invalid paired client records")?
            }
        };
        // Old Sunshine stored a list of certificates per anonymous device.
        // Consume it once; otherwise a subsequent unpair would resurrect it.
        if let Some(devices) = root.get("devices").and_then(Value::as_array) {
            for device in devices {
                if let Some(certs) = device.get("certs").and_then(Value::as_array) {
                    for cert in certs {
                        let cert = cert.as_str().context("invalid legacy certificate")?;
                        if !clients.iter().any(|c| c.cert == cert) {
                            clients.push(Client {
                                name: "Imported client".into(),
                                cert: cert.into(),
                                uuid: uuid::Uuid::new_v4().to_string(),
                                perm: default_permission(),
                                enabled: true,
                                extra: BTreeMap::new(),
                            });
                        }
                    }
                }
            }
            root.as_object_mut().unwrap().remove("devices");
        }
        let mut certs = HashSet::new();
        let mut ids = HashSet::new();
        for c in &clients {
            if !certs.insert(c.der()?) || !ids.insert(c.uuid.clone()) {
                bail!("duplicate paired client identity");
            }
        }
        if clients.len() > 256 {
            bail!("too many paired clients");
        }
        Ok(Self {
            document,
            clients,
            unique_id,
        })
    }
    pub fn client_by_certificate(&self, der: &[u8]) -> Option<&Client> {
        let mut found = None;
        for c in &self.clients {
            if c.enabled && c.perm != 0 && c.der().is_ok_and(|b| crypto::equal(&b, der)) {
                if found.is_some() {
                    return None;
                }
                found = Some(c);
            }
        }
        found
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut d = self.document.clone();
        d["root"]["uniqueid"] = json!(self.unique_id);
        d["root"]["named_devices"] = serde_json::to_value(&self.clients)?;
        write_json(path, &d)
    }
    pub fn add(&mut self, path: &Path, client: Client) -> Result<()> {
        if self.clients.len() >= 256
            || self
                .clients
                .iter()
                .any(|c| c.uuid == client.uuid || c.der().ok() == client.der().ok())
        {
            bail!("duplicate client or paired client limit reached");
        }
        let mut next = self.clone();
        next.clients.push(client);
        next.save(path)?;
        *self = next;
        Ok(())
    }
    pub fn remove(&mut self, path: &Path, id: &str) -> Result<()> {
        let mut next = self.clone();
        next.clients.retain(|c| c.uuid != id);
        next.save(path)?;
        *self = next;
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
    pub salt: String,
}
impl Credentials {
    pub fn new(username: String, password: &str) -> Result<Self> {
        if username.is_empty() || username.len() > 128 || password.len() < 8 {
            bail!("username required; password must have at least 8 characters");
        }
        let salt = hex::encode(crypto::random::<8>());
        Ok(Self {
            username,
            password: crypto::legacy_hash(format!("{password}{salt}").as_bytes()),
            salt,
        })
    }
    pub fn verifies(&self, username: &str, password: &str) -> bool {
        self.username.eq_ignore_ascii_case(username)
            && crypto::matches_hash(
                format!("{password}{}", self.salt).as_bytes(),
                &self.password,
            )
    }
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct App {
    pub name: String,
    #[serde(default)]
    pub cmd: String,
    #[serde(default, rename = "working-dir")]
    pub working_dir: String,
    #[serde(default, rename = "prep-cmd")]
    pub prep: Vec<PrepCommand>,
    #[serde(skip)]
    pub computed_id: Option<u32>,
    #[serde(skip)]
    pub aliases: Vec<u32>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PrepCommand {
    #[serde(default)]
    pub r#do: String,
    #[serde(default)]
    pub undo: String,
    #[serde(default, deserialize_with = "compatible_bool")]
    pub elevated: bool,
}
impl App {
    pub fn id(&self) -> u32 {
        if let Some(id) = self.computed_id {
            return id;
        }
        if let Some(n) = self.extra.get("id").and_then(Value::as_u64) {
            return n as u32;
        }
        (crc32fast::hash(self.name.as_bytes()) as i32).unsigned_abs()
    }
    pub fn desktop() -> Self {
        Self {
            name: "Desktop".into(),
            cmd: String::new(),
            working_dir: String::new(),
            prep: vec![],
            computed_id: None,
            aliases: vec![],
            extra: BTreeMap::new(),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bad_existing_state_is_not_replaced() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        std::fs::write(&p, b"broken").unwrap();
        assert!(PairedState::load(&p).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"broken");
    }
    #[test]
    fn atomic_state_replacement_and_unknown_fields() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        write_json(
            &p,
            &json!({"root":{"uniqueid":"same-id","named_devices":[],"other":[1,2]}}),
        )
        .unwrap();
        let s = PairedState::load(&p).unwrap();
        s.save(&p).unwrap();
        let n = load_json(&p, json!(null)).unwrap();
        assert_eq!(n["root"]["other"], json!([1, 2]));
        assert_eq!(n["root"]["uniqueid"], "same-id");
    }
    #[test]
    fn existing_credentials_format() {
        let c = Credentials {
            username: "user".into(),
            salt: "abc".into(),
            password: hex::encode(crypto::hash(b"passwordabc")).to_uppercase(),
        };
        assert!(c.verifies("user", "password"));
        assert!(c.verifies("User", "password"));
        assert!(!c.verifies("other", "password"));
        assert!(!c.verifies("user", "wrong"));
        // Independent SHA-256/password+salt vector in C++ util::hex byte order.
        let previous = Credentials {
            password: "F88D961F52F30D505CC0CBB98D01B38D0D789C075812B3C38748CEEAFFB73367".into(),
            ..c
        };
        assert!(previous.verifies("USER", "password"));
        assert!(!previous.verifies("user", "wrong"));
        assert!(!previous.verifies("other", "password"));
        let current = Credentials::new("user".into(), "new-password").unwrap();
        assert_eq!(
            current.password,
            crypto::legacy_hash(format!("new-password{}", current.salt).as_bytes())
        );
        assert!(current.verifies("user", "new-password"));
    }
}
