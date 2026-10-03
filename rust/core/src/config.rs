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
    /// Read `sunshine.conf` as Vibepollo does, so an imported file never stops
    /// the host: a line without `=` or with an unusable key is skipped with a
    /// warning, characters before a key (a UTF-8 byte order mark) are dropped,
    /// the first of duplicate keys wins and an empty value keeps the default.
    /// `#` starts a comment outside quotes, and a value opening `[` or `{`
    /// continues until its brackets close.
    pub fn parse(text: &str) -> Result<Self> {
        let mut values = BTreeMap::new();
        let mut lines = text.lines().enumerate();
        while let Some((number, raw)) = lines.next() {
            let number = number + 1;
            if raw.contains('\0') {
                tracing::warn!(line = number, "configuration line contains NUL; skipped");
                continue;
            }
            let line = strip_comment(raw);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                tracing::warn!(line = number, "configuration line has no '='; skipped");
                continue;
            };
            let key = key
                .trim()
                .trim_start_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '_'));
            let mut value = value.trim().to_owned();
            if value.starts_with(['[', '{']) {
                let mut depth = bracket_depth(&value);
                while depth > 0 {
                    let Some((_, next)) = lines.next() else {
                        break;
                    };
                    let next = strip_comment(next);
                    depth += bracket_depth(&next);
                    value.push('\n');
                    value.push_str(next.trim());
                }
                if depth != 0 {
                    tracing::warn!(
                        line = number,
                        key,
                        "configuration list is not closed; skipped"
                    );
                    continue;
                }
            }
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                tracing::warn!(line = number, key, "invalid configuration key; skipped");
                continue;
            }
            if value.is_empty() {
                continue;
            }
            if values.contains_key(key) {
                tracing::warn!(
                    line = number,
                    key,
                    "configuration key repeated; the first value is used"
                );
                continue;
            }
            values.insert(key.to_owned(), value);
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
    /// The value of `key`, or `default` when it is unset or empty.
    pub fn get<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.values
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(default)
    }
    pub fn boolean(&self, key: &str, default: bool) -> bool {
        self.values
            .get(key)
            .and_then(|s| match unquote(s.trim()).to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" | "enable" | "enabled" | "on" => Some(true),
                "false" | "no" | "0" | "disable" | "disabled" | "off" => Some(false),
                _ => None,
            })
            .unwrap_or(default)
    }
    /// An integer setting; Vibepollo also writes quoted (`"756"`) and
    /// hexadecimal (`0x2a`) values.
    pub fn integer(&self, key: &str, default: i64) -> i64 {
        let Some(value) = self.values.get(key).filter(|v| !v.trim().is_empty()) else {
            return default;
        };
        parse_integer(value).unwrap_or_else(|| {
            invalid(key, value);
            default
        })
    }
    /// A list setting. JSON arrays and Vibepollo's `[a, b]` or `a, b` lists
    /// are accepted; numbers are returned as written.
    pub fn list(&self, key: &str) -> Vec<String> {
        let Some(value) = self.values.get(key) else {
            return Vec::new();
        };
        if let Ok(serde_json::Value::Array(items)) = serde_json::from_str(value) {
            return items
                .into_iter()
                .map(|item| match item {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                })
                .filter(|s| !s.trim().is_empty())
                .collect();
        }
        let inner = value.trim();
        let inner = inner.strip_prefix('[').unwrap_or(inner);
        let inner = inner.strip_suffix(']').unwrap_or(inner);
        let mut items = Vec::new();
        let (mut depth, mut quote, mut start) = (0i32, false, 0);
        for (i, c) in inner.char_indices() {
            match c {
                '"' => quote = !quote,
                '[' | '{' if !quote => depth += 1,
                ']' | '}' if !quote => depth -= 1,
                ',' if !quote && depth == 0 => {
                    items.push(&inner[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        items.push(&inner[start..]);
        items
            .into_iter()
            .map(|item| unquote(item.trim()).to_owned())
            .filter(|item| !item.is_empty())
            .collect()
    }
    pub fn log_level(&self) -> &'static str {
        match self
            .get("min_log_level", "info")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "0" | "verbose" | "trace" => "trace",
            "1" | "debug" => "debug",
            "2" | "info" => "info",
            "3" | "warning" | "warn" => "warn",
            "4" | "5" | "error" | "fatal" => "error",
            "6" | "none" | "off" => "off",
            _ => "info",
        }
    }
    /// The base port. An out-of-range port is ignored, as in Vibepollo.
    pub fn port(&self) -> Result<u16> {
        let n = self.integer("port", 47989);
        if !PORTS.contains(&n) {
            invalid("port", self.get("port", ""));
            return Ok(47989);
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
            // Legacy PATCH and POST both reset a setting when its value is
            // null/empty. Keeping the literal "null" silently defeats defaults.
            if value.is_null() || value.as_str() == Some("") {
                next.values.remove(key);
                continue;
            }
            let value = match value {
                serde_json::Value::String(v) => v.clone(),
                _ => value.to_string(),
            };
            if value.contains('\0') {
                bail!("configuration cannot contain NUL");
            }
            if value.contains(['\r', '\n'])
                && !(value.trim().starts_with('[') || value.trim().starts_with('{'))
            {
                bail!("configuration values cannot inject new keys");
            }
            next.values.insert(key.clone(), value);
        }
        if next.values.contains_key("port") && !PORTS.contains(&next.integer("port", 0)) {
            bail!("port must be between 1029 and 65514");
        }
        crate::framegen::Rate::parse(next.get("frame_limiter_fps_limit", "0"))?;
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
        let mut request = self.display_request_rate(
            width,
            height,
            crate::framegen::Rate(fps.saturating_mul(1000)),
            false,
            false,
        )?;
        request.refresh = request.refresh.map(|r| r.saturating_add(500) / 1000);
        Ok(request)
    }
    pub fn display_request_rate(
        &self,
        width: u32,
        height: u32,
        rate: crate::framegen::Rate,
        hdr: bool,
        virtual_display: bool,
    ) -> Result<DisplayRequest> {
        if !virtual_display && self.get("dd_configuration_option", "verify_only") == "disabled" {
            return Ok(DisplayRequest {
                resolution: None,
                refresh: None,
                prefer_highest: false,
                hdr: None,
            });
        }
        let resolution_option = self.get("dd_resolution_option", "auto");
        let refresh_option = self.get("dd_refresh_rate_option", "auto");
        let resolution_option = match resolution_option {
            "disabled" | "auto" | "manual" => resolution_option,
            other => {
                fallback("dd_resolution_option", other, "auto");
                "auto"
            }
        };
        let refresh_option = match refresh_option {
            "disabled" | "auto" | "manual" | "prefer_highest" => refresh_option,
            other => {
                fallback("dd_refresh_rate_option", other, "auto");
                "auto"
            }
        };
        let mut resolution = match resolution_option {
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
        let mut refresh = match refresh_option {
            "disabled" => None,
            "auto" => Some(rate.0),
            "manual" => {
                let rate = crate::framegen::Rate::parse(self.get("dd_manual_refresh_rate", ""))?;
                if rate.0 < 1000 {
                    bail!("invalid manual refresh rate");
                }
                Some(rate.0)
            }
            "prefer_highest" => None,
            _ => bail!("invalid display refresh policy"),
        };
        let remapping_type = match (resolution_option == "auto", refresh_option == "auto") {
            (true, true) => Some("mixed"),
            (true, false) => Some("resolution_only"),
            (false, true) => Some("refresh_rate_only"),
            _ => None,
        };
        if let Some(kind) = remapping_type
            && let Some(value) = self.values.get("dd_mode_remapping")
        {
            let mappings: serde_json::Value =
                serde_json::from_str(value).context("invalid display mode remapping")?;
            if let Some(entries) = mappings.get(kind).and_then(serde_json::Value::as_array) {
                for entry in entries {
                    let text = |key: &str| {
                        entry
                            .get(key)
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .trim()
                    };
                    let parse_resolution = |s: &str| -> Result<(u32, u32)> {
                        let (w, h) = s
                            .split_once('x')
                            .context("remapping resolution must be WIDTHxHEIGHT")?;
                        Ok((w.trim().parse()?, h.trim().parse()?))
                    };
                    if kind != "refresh_rate_only"
                        && !text("requested_resolution").is_empty()
                        && resolution != Some(parse_resolution(text("requested_resolution"))?)
                    {
                        continue;
                    }
                    if kind != "resolution_only"
                        && !text("requested_fps").is_empty()
                        && refresh != Some(crate::framegen::Rate::parse(text("requested_fps"))?.0)
                    {
                        continue;
                    }
                    if kind != "refresh_rate_only" && !text("final_resolution").is_empty() {
                        resolution = Some(parse_resolution(text("final_resolution"))?);
                    }
                    if kind != "resolution_only" && !text("final_refresh_rate").is_empty() {
                        refresh = Some(crate::framegen::Rate::parse(text("final_refresh_rate"))?.0);
                    }
                    break;
                }
            }
        }
        if resolution.is_some_and(|(w, h)| !(320..=7680).contains(&w) || !(200..=4320).contains(&h))
            || refresh.is_some_and(|f| !(1000..=1_000_000).contains(&f))
        {
            bail!("display mode is outside its limits");
        }
        Ok(DisplayRequest {
            resolution,
            refresh,
            prefer_highest: refresh_option == "prefer_highest",
            hdr: if self.boolean("rtx_hdr", false) {
                Some(false)
            } else if self.get("dd_hdr_option", "auto") == "disabled" {
                None
            } else {
                Some(match self.get("dd_hdr_request_override", "auto") {
                    "force_on" => true,
                    "force_off" => false,
                    other => {
                        fallback("dd_hdr_request_override", other, "auto");
                        hdr
                    }
                })
            },
        })
    }
}
/// Base ports whose derived ports (base - 5 to base + 21) are all valid.
const PORTS: std::ops::RangeInclusive<i64> = 1029..=65514;
/// Report a setting value the host does not understand and will not use.
/// Each key and value is reported once.
pub fn invalid(key: &str, value: &str) {
    static SEEN: std::sync::Mutex<std::collections::BTreeSet<(String, String)>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());
    if SEEN
        .lock()
        .unwrap()
        .insert((key.to_owned(), value.to_owned()))
    {
        tracing::warn!(key, value, "unsupported setting value; using the default");
    }
}
/// Report `value` unless it is the default the caller falls back to.
pub fn fallback(key: &str, value: &str, default: &str) {
    if value != default {
        invalid(key, value);
    }
}
fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}
pub fn parse_integer(value: &str) -> Option<i64> {
    let value = unquote(value.trim()).trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return i64::from_str_radix(hex, 16).ok();
    }
    value.parse().ok()
}
/// The line without its comment. Quotes protect `#` only when they are
/// balanced on the line; otherwise `#` always starts a comment, as in Vibepollo.
fn strip_comment(line: &str) -> String {
    let mut quote = false;
    let mut escaped = false;
    let mut cut = None;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if quote => escaped = true,
            '"' => quote = !quote,
            '#' if !quote && cut.is_none() => cut = Some(i),
            _ => {}
        }
    }
    let cut = if quote { line.find('#') } else { cut };
    line[..cut.unwrap_or(line.len())].to_owned()
}
/// Net bracket depth of a line, ignoring brackets in balanced quotes.
fn bracket_depth(line: &str) -> i32 {
    let balanced = line.matches('"').count().is_multiple_of(2);
    let (mut depth, mut quote, mut escaped) = (0, false, false);
    for c in line.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if quote => escaped = true,
            '"' if balanced => quote = !quote,
            '[' | '{' if !quote => depth += 1,
            ']' | '}' if !quote => depth -= 1,
            _ => {}
        }
    }
    depth
}
pub struct DisplayRequest {
    pub resolution: Option<(u32, u32)>,
    pub refresh: Option<u32>,
    pub prefer_highest: bool,
    pub hdr: Option<bool>,
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
        for (legacy, expected) in [
            ("debug", "debug"),
            ("1", "debug"),
            ("warning", "warn"),
            ("verbose", "trace"),
            ("fatal", "error"),
            ("none", "off"),
            ("invalid", "info"),
        ] {
            assert_eq!(
                Config::parse(&format!("min_log_level={legacy}\n"))
                    .unwrap()
                    .log_level(),
                expected
            );
        }
    }
    #[test]
    fn exact_display_remapping_hdr_override_and_disabled_policy() {
        let c = Config::parse("dd_mode_remapping={\"mixed\":[{\"requested_resolution\":\"1920x1080\",\"requested_fps\":\"59.94\",\"final_resolution\":\"2560x1440\",\"final_refresh_rate\":\"119.88\"}]}\ndd_hdr_request_override=force_off\n").unwrap();
        let r = c
            .display_request_rate(1920, 1080, crate::framegen::Rate(59940), true, false)
            .unwrap();
        assert_eq!(r.resolution, Some((2560, 1440)));
        assert_eq!(r.refresh, Some(119880));
        assert_eq!(r.hdr, Some(false));
        let c = Config::parse("dd_configuration_option=disabled\ndd_refresh_rate_option=manual\ndd_manual_refresh_rate=119.88\n").unwrap();
        let r = c
            .display_request_rate(1920, 1080, crate::framegen::Rate(59940), true, false)
            .unwrap();
        assert_eq!((r.resolution, r.refresh, r.hdr), (None, None, None));
        assert_eq!(
            c.display_request_rate(1920, 1080, crate::framegen::Rate(59940), true, true)
                .unwrap()
                .refresh,
            Some(119880)
        );
    }
    #[test]
    fn vibepollo_files_load_like_vibepollo() {
        let c = Config::parse(concat!(
            "\u{feff}sunshine_name = 27\" PC # living room\n",
            "not a setting\n",
            "keybindings = [0x10, 0xA0, \"0x11\", 0xA2]\n",
            "csrf_allowed_origins = [https://a.example, https://b.example]\n",
            "encoder =\n",
            "port = \"48123\"\n",
            "port = 50000\n",
            "audio_sink = Speakers\n",
            "bad key = 1\n",
            "global_prep_cmd = [\n",
            "  {\"do\":\"echo #1\",\"undo\":\"\"}\n",
            "]\n",
            "dd_snapshot_exclude_devices = DISPLAY1, \"DISPLAY2\"\n",
            "fec_percentage = 0x1e\n",
            "enable_pairing = enable\n",
        ))
        .unwrap();
        assert_eq!(c.get("sunshine_name", ""), "27\" PC");
        assert_eq!(c.list("keybindings"), ["0x10", "0xA0", "0x11", "0xA2"]);
        assert_eq!(
            c.list("csrf_allowed_origins"),
            ["https://a.example", "https://b.example"]
        );
        assert_eq!(c.get("encoder", "auto"), "auto");
        assert!(!c.values.contains_key("encoder"));
        assert_eq!(c.port().unwrap(), 48123);
        assert_eq!(c.get("audio_sink", ""), "Speakers");
        assert!(!c.values.contains_key("bad key") && !c.values.contains_key("key"));
        assert_eq!(
            c.get("global_prep_cmd", ""),
            "[\n{\"do\":\"echo #1\",\"undo\":\"\"}\n]"
        );
        assert_eq!(
            c.list("dd_snapshot_exclude_devices"),
            ["DISPLAY1", "DISPLAY2"]
        );
        assert_eq!(c.integer("fec_percentage", 20), 30);
        assert!(c.boolean("enable_pairing", false));
        assert_eq!(
            Config::parse("port = 70000\n").unwrap().port().unwrap(),
            47989
        );
        // An unclosed list is dropped; the rest of the file still loads.
        let c = Config::parse("keybindings = [0x10,\nport = 48123\n").unwrap();
        assert!(c.values.is_empty());
        let c = Config::parse("port = 48123\nkeybindings = [0x10,\n").unwrap();
        assert_eq!(c.port().unwrap(), 48123);
        assert!(!c.values.contains_key("keybindings"));
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
