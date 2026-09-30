use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Retain all keys, including extension keys a newer UI or client may write.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub values: BTreeMap<String, String>,
}
impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let mut values = BTreeMap::new();
        let mut pending = String::new();
        let mut depth = 0i32;
        let mut quote = false;
        let mut escaped = false;
        for line in text.lines() {
            let mut clean = String::new();
            for c in line.chars() {
                if escaped {
                    escaped = false;
                    clean.push(c);
                    continue;
                }
                if c == '\\' && quote {
                    escaped = true;
                    clean.push(c);
                    continue;
                }
                if c == '"' {
                    quote = !quote;
                }
                if c == '#' && !quote {
                    break;
                }
                if !quote {
                    match c {
                        '[' | '{' => depth += 1,
                        ']' | '}' => depth -= 1,
                        _ => {}
                    }
                }
                if depth < 0 {
                    bail!("unbalanced configuration value");
                }
                clean.push(c);
            }
            if clean.trim().is_empty() && pending.is_empty() {
                continue;
            }
            if !pending.is_empty() {
                pending.push('\n');
            }
            pending.push_str(clean.trim());
            if depth == 0 && !quote {
                let (key, value) = pending
                    .split_once('=')
                    .context("configuration line is missing '='")?;
                let key = key.trim();
                if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    bail!("invalid configuration key: {key}");
                }
                values.insert(key.to_owned(), value.trim().to_owned());
                pending.clear();
            }
        }
        if depth != 0 || quote || !pending.is_empty() {
            bail!("unterminated configuration value");
        }
        Ok(Self { values })
    }
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::parse(&s).with_context(|| format!("reading {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn get<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.values.get(key).map(String::as_str).unwrap_or(default)
    }
    pub fn boolean(&self, key: &str, default: bool) -> bool {
        self.values
            .get(key)
            .map(|s| matches!(s.trim(), "true" | "yes" | "1" | "enabled"))
            .unwrap_or(default)
    }
    pub fn integer(&self, key: &str, default: i64) -> i64 {
        self.values
            .get(key)
            .and_then(|s| s.parse().ok())
            .unwrap_or(default)
    }
    pub fn port(&self) -> Result<u16> {
        let n = self.integer("port", 47989);
        if !(1029..=65514).contains(&n) {
            bail!("port must be between 1029 and 65514");
        }
        Ok(n as u16)
    }
    pub fn ports(&self) -> Result<Ports> {
        Ok(Ports::from_base(self.port()?))
    }
    pub fn text(&self) -> String {
        self.values
            .iter()
            .map(|(k, v)| format!("{k} = {v}\n"))
            .collect()
    }
    pub fn json(&self) -> serde_json::Value {
        serde_json::Value::Object(
            self.values
                .iter()
                .map(|(k, v)| {
                    let parsed = serde_json::from_str(v)
                        .unwrap_or_else(|_| serde_json::Value::String(v.clone()));
                    (k.clone(), parsed)
                })
                .collect(),
        )
    }
    pub fn update(&mut self, object: &serde_json::Map<String, serde_json::Value>) -> Result<()> {
        let mut next = self.clone();
        for (key, value) in object {
            if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || key.is_empty() {
                bail!("invalid configuration key");
            }
            let value = match value {
                serde_json::Value::String(v) => v.clone(),
                _ => value.to_string(),
            };
            if value.contains(['\r', '\n'])
                && !(value.trim().starts_with('[') || value.trim().starts_with('{'))
            {
                bail!("configuration values cannot inject new keys");
            }
            next.values.insert(key.clone(), value);
        }
        next.ports()?;
        *self = next;
        Ok(())
    }
    pub fn path(&self, key: &str, directory: &Path, default: &str) -> PathBuf {
        let p = PathBuf::from(self.get(key, default));
        if p.is_absolute() {
            p
        } else {
            directory.join(p)
        }
    }
    pub fn display_request(&self, width: u32, height: u32, fps: u32) -> Result<DisplayRequest> {
        let resolution = match self.get("dd_resolution_option", "disabled") {
            "disabled" => None,
            "auto" => Some((width, height)),
            "manual" => {
                let (width, height) = self
                    .get("dd_manual_resolution", "")
                    .split_once('x')
                    .context("manual resolution must be WIDTHxHEIGHT")?;
                Some((width.trim().parse()?, height.trim().parse()?))
            }
            _ => bail!("invalid display resolution policy"),
        };
        let refresh = match self.get("dd_refresh_rate_option", "disabled") {
            "disabled" => None,
            "auto" => Some(fps),
            "manual" => {
                let rate: f64 = self.get("dd_manual_refresh_rate", "").parse()?;
                if !rate.is_finite() || !(1.0..=1000.0).contains(&rate) {
                    bail!("invalid manual refresh rate");
                }
                Some(rate.round() as u32)
            }
            _ => bail!("invalid display refresh policy"),
        };
        if resolution.is_some_and(|(w, h)| !(320..=7680).contains(&w) || !(200..=4320).contains(&h))
            || refresh.is_some_and(|f| f == 0 || f > 1000)
        {
            bail!("display mode is outside its limits");
        }
        Ok(DisplayRequest {
            resolution,
            refresh,
        })
    }
}
pub struct DisplayRequest {
    pub resolution: Option<(u32, u32)>,
    pub refresh: Option<u32>,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Ports {
    pub http: u16,
    pub https: u16,
    pub web: u16,
    pub video: u16,
    pub control: u16,
    pub audio: u16,
    pub rtsp: u16,
}
impl Ports {
    pub fn from_base(n: u16) -> Self {
        Self {
            http: n,
            https: n - 5,
            web: n + 1,
            video: n + 9,
            control: n + 10,
            audio: n + 11,
            rtsp: n + 21,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_policies_keep_resolution_and_refresh_independent() {
        let config =
            Config::parse("dd_resolution_option = disabled\ndd_refresh_rate_option = auto\n")
                .unwrap();
        let requested = config.display_request(1920, 1080, 120).unwrap();
        assert_eq!(requested.resolution, None);
        assert_eq!(requested.refresh, Some(120));
        let config = Config::parse("dd_resolution_option = manual\ndd_manual_resolution = 2560x1440\ndd_refresh_rate_option = disabled\n").unwrap();
        let requested = config.display_request(1920, 1080, 120).unwrap();
        assert_eq!(requested.resolution, Some((2560, 1440)));
        assert_eq!(requested.refresh, None);
        for value in ["NaN", "inf", "0", "1001"] {
            let config = Config::parse(&format!(
                "dd_refresh_rate_option = manual\ndd_manual_refresh_rate = {value}\n"
            ))
            .unwrap();
            assert!(config.display_request(1920, 1080, 60).is_err());
        }
    }
    #[test]
    fn existing_config_round_trips() {
        let c = Config::parse("# Apollo\nport=48123\nunknown_key = custom\nprep_cmd = [\n {\"do\":\"echo #kept\",\"undo\":\"\"}\n]\n").unwrap();
        assert_eq!(c.ports().unwrap().rtsp, 48144);
        assert_eq!(Config::parse(&c.text()).unwrap().values, c.values);
        assert_eq!(c.get("unknown_key", ""), "custom");
    }
    #[test]
    fn update_is_transactional_and_rejects_injection() {
        let mut c = Config::default();
        assert!(
            c.update(
                serde_json::json!({"port":65535,"encoder":"nvenc"})
                    .as_object()
                    .unwrap()
            )
            .is_err()
        );
        assert!(c.values.is_empty());
        assert!(
            c.update(
                serde_json::json!({"encoder":"software\nport=1"})
                    .as_object()
                    .unwrap()
            )
            .is_err()
        );
    }
}
