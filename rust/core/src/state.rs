use crate::{config::Config, crypto};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

/// The JSON in a state file, as Vibepollo reads it: a UTF-8 byte order mark
/// is dropped, and a blank file holds nothing (None).
pub fn json_bytes(bytes: &[u8]) -> Option<&[u8]> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    bytes
        .iter()
        .any(|b| !b" \t\r\n\x0b\x0c".contains(b))
        .then_some(bytes)
}
/// Never turn an unreadable existing identity file into a fresh empty host.
/// A blank file has nothing to lose and reads as a missing one.
pub fn load_json(path: &Path, default: Value) -> Result<Value> {
    match std::fs::read(path) {
        Ok(b) => match json_bytes(&b) {
            Some(json) => serde_json::from_slice(json)
                .with_context(|| format!("invalid JSON in {}", path.display())),
            None => Ok(default),
        },
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
    /// Whether this device may run its connect and disconnect commands
    /// (on unless the administrator turned it off, as in Vibepollo).
    pub fn allows_commands(&self) -> bool {
        !self
            .extra
            .get("allow_client_commands")
            .is_some_and(|v| v == false || v == "false")
    }
    pub fn der(&self) -> Result<Vec<u8>> {
        crypto::certificate_der(&self.cert)
    }
}
/// Sunshine's and Apollo's property-tree writer stored an empty list or
/// object as "". Only the known containers are read back as empty, as in
/// Vibepollo; every other value is kept as it is.
fn empty_containers(root: &mut Value) {
    let list = |value: &mut Value| {
        if *value == "" {
            *value = json!([]);
        }
    };
    if let Some(devices) = root.get_mut("named_devices") {
        list(devices);
        for device in devices.as_array_mut().into_iter().flatten() {
            for key in ["do", "undo"] {
                if let Some(commands) = device.get_mut(key) {
                    list(commands);
                }
            }
            if let Some(overrides) = device.get_mut("config_overrides")
                && *overrides == ""
            {
                *overrides = json!({});
            }
        }
    }
    if let Some(devices) = root.get_mut("devices") {
        list(devices);
        for device in devices.as_array_mut().into_iter().flatten() {
            if let Some(certs) = device.get_mut("certs") {
                list(certs);
            }
        }
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
        empty_containers(root);
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
        // Vibepollo appended a device again when it paired again, so an
        // imported profile can list one certificate twice. Keep the first
        // entry rather than refusing to start.
        let mut certs = HashSet::new();
        let mut ids = HashSet::new();
        let mut unique = Vec::with_capacity(clients.len());
        for mut c in clients {
            // Such a device could never connect; it must not keep every
            // other device from loading.
            let der = match c.der() {
                Ok(der) => der,
                Err(error) => {
                    tracing::warn!(
                        device = %c.name,
                        error = %format!("{error:#}"),
                        "paired device has an unreadable certificate; skipped"
                    );
                    continue;
                }
            };
            if !certs.insert(der) {
                continue;
            }
            if !ids.insert(c.uuid.clone()) {
                c.uuid = uuid::Uuid::new_v4().to_string();
                ids.insert(c.uuid.clone());
            }
            unique.push(c);
        }
        let clients = unique;
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
    /// Pair a device. A certificate that is already paired, as when Moonlight
    /// forgot this host and pairs again, keeps its entry, identity and
    /// permissions; only the name is updated. Refusing it left the device
    /// unable to pair until someone deleted it in the console.
    pub fn add(&mut self, path: &Path, client: Client) -> Result<()> {
        let der = client.der()?;
        let mut next = self.clone();
        if let Some(existing) = next
            .clients
            .iter_mut()
            .find(|c| c.der().is_ok_and(|b| crypto::equal(&b, &der)))
        {
            existing.name = client.name;
        } else {
            if next.clients.len() >= 256 || next.clients.iter().any(|c| c.uuid == client.uuid) {
                bail!("duplicate client or paired client limit reached");
            }
            next.clients.push(client);
        }
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
/// Where a profile keeps its files, as its sunshine.conf names them.
pub struct ProfileFiles {
    pub paired: PathBuf,
    pub credentials: PathBuf,
    pub apps: PathBuf,
    pub aliases: PathBuf,
    pub certificate: PathBuf,
    pub key: PathBuf,
}
impl ProfileFiles {
    pub fn new(config: &Config, directory: &Path) -> Self {
        let credentials = if directory.join("sunshine_credentials.json").exists() {
            "sunshine_credentials.json"
        } else {
            "sunshine_state.json"
        };
        Self {
            paired: config.path("file_state", directory, "sunshine_state.json"),
            credentials: config.path("credentials_file", directory, credentials),
            apps: config.path("file_apps", directory, "apps.json"),
            aliases: config.path("vibeshine_file_state", directory, "vibeshine_state.json"),
            certificate: config.path("cert", directory, "credentials/cacert.pem"),
            key: config.path("pkey", directory, "credentials/cakey.pem"),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
    pub salt: String,
}
impl Credentials {
    /// The web console's credentials in `path`; None until they are set.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let document = load_json(path, json!({}))?;
        if document.get("username").is_none() {
            return Ok(None);
        }
        Ok(Some(
            serde_json::from_value(document).context("invalid web credentials")?,
        ))
    }
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
    fn a_byte_order_mark_is_skipped_and_a_blank_file_reads_as_missing() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        std::fs::write(&p, b"\xef\xbb\xbf{\"root\":{\"uniqueid\":\"same-id\"}}").unwrap();
        assert_eq!(
            load_json(&p, json!(null)).unwrap(),
            json!({"root":{"uniqueid":"same-id"}})
        );
        assert_eq!(PairedState::load(&p).unwrap().unique_id, "same-id");
        for blank in [&b""[..], b"\xef\xbb\xbf", b" \r\n\t"] {
            std::fs::write(&p, blank).unwrap();
            assert_eq!(
                load_json(&p, json!({"apps":[]})).unwrap(),
                json!({"apps":[]})
            );
            assert!(PairedState::load(&p).unwrap().clients.is_empty());
        }
        std::fs::write(&p, b"\xef\xbb\xbf broken").unwrap();
        assert!(load_json(&p, json!(null)).is_err());
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
    fn pairing_a_known_certificate_again_keeps_the_device_and_duplicates_load() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        let identity = crypto::Identity::generate().unwrap();
        let client = |name: &str, uuid: &str| Client {
            name: name.into(),
            cert: identity.certificate.clone(),
            uuid: uuid.into(),
            perm: 0x0300_0000,
            enabled: true,
            extra: BTreeMap::new(),
        };
        let mut s = PairedState::load(&p).unwrap();
        s.add(&p, client("Phone", "a")).unwrap();
        s.clients[0].perm = 0x071f_1f00;
        s.add(&p, client("Phone again", "b")).unwrap();
        assert_eq!(s.clients.len(), 1);
        assert_eq!(s.clients[0].uuid, "a");
        assert_eq!(s.clients[0].perm, 0x071f_1f00);
        assert_eq!(s.clients[0].name, "Phone again");
        // A Vibepollo profile that lists the certificate twice still loads.
        let mut doubled = s.clone();
        doubled.clients.push(client("Phone", "a"));
        doubled.save(&p).unwrap();
        let loaded = PairedState::load(&p).unwrap();
        assert_eq!(loaded.clients.len(), 1);
        let der = loaded.clients[0].der().unwrap();
        assert!(loaded.client_by_certificate(&der).is_some());
    }
    #[test]
    fn property_tree_empty_lists_and_unreadable_certificates_load() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        let write = |document: Value| std::fs::write(&p, document.to_string()).unwrap();
        write(json!({"root":{"uniqueid":"same-id","named_devices":"","devices":""}}));
        let s = PairedState::load(&p).unwrap();
        assert!(s.clients.is_empty());
        assert_eq!(s.unique_id, "same-id");
        let identity = crypto::Identity::generate().unwrap();
        write(json!({"root":{"uniqueid":"same-id","named_devices":[
            {"name":"Broken","cert":"not a certificate","uuid":"a"},
            {"name":"Phone","cert":identity.certificate,"uuid":"b",
             "do":"","undo":"","config_overrides":"","other":""}
        ],"devices":[{"certs":""}]}}));
        let s = PairedState::load(&p).unwrap();
        assert_eq!(s.clients.len(), 1);
        let phone = &s.clients[0];
        assert_eq!(phone.name, "Phone");
        assert_eq!(phone.extra["do"], json!([]));
        assert_eq!(phone.extra["undo"], json!([]));
        assert_eq!(phone.extra["config_overrides"], json!({}));
        assert_eq!(phone.extra["other"], "");
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
