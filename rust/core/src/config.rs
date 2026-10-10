//! The host configuration: `sunshine.conf`-compatible `key = value` settings,
//! typed accessors with Vibepollo's defaults, per-app and per-client overrides,
//! and the port layout derived from the base port.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Setting values Rubylight no longer offers, and what replaces each:
/// the ViGEmBus controllers, which Sunshine, Apollo, Vibepollo and earlier
/// Rubylight profiles can carry, became the VHF pad of the same family.
pub const RETIRED_VALUES: &[(&str, &str, &str)] = &[
    ("gamepad", "x360", "vhf_xbox_one"),
    ("gamepad", "ds4", "vhf_ds4"),
];
/// The current value for a retired `value` of `key`, if it is one.
pub fn replacement(key: &str, value: &str) -> Option<&'static str> {
    RETIRED_VALUES
        .iter()
        .find(|(k, old, _)| *k == key && *old == value)
        .map(|(_, _, new)| *new)
}

/// Replace retired values among an app's or a device's settings, which are
/// JSON. Returns whether anything changed.
pub fn replace_retired<'a>(
    settings: impl IntoIterator<Item = (&'a String, &'a mut serde_json::Value)>,
) -> bool {
    let mut changed = false;
    for (key, value) in settings {
        if let Some(current) = value.as_str().and_then(|old| replacement(key, old)) {
            *value = serde_json::Value::from(current);
            changed = true;
        }
    }
    changed
}

/// Replace retired values in a library app: its top-level settings, as
/// Apollo and Vibepollo store them, and its `config-overrides`. Returns
/// whether anything changed.
pub fn replace_retired_in_app(app: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    let mut changed = replace_retired(app.iter_mut());
    if let Some(overrides) = app
        .get_mut("config-overrides")
        .and_then(serde_json::Value::as_object_mut)
    {
        changed |= replace_retired(overrides.iter_mut());
    }
    changed
}

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
            // The console then shows, and the next save writes, the
            // replacement.
            if let Some(current) = replacement(key, &value) {
                tracing::info!(
                    key,
                    retired = value,
                    current,
                    "retired setting value replaced"
                );
                value = current.to_owned();
            }
            values.insert(key.to_owned(), value);
        }
        Ok(Self { values })
    }
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                Self::parse(&decode(&bytes)).with_context(|| format!("reading {}", path.display()))
            }
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
    /// `virtual_display_mode`; unset, it is per client on Windows 11 and
    /// disabled on Windows 10, as in Vibepollo.
    pub fn virtual_display_mode(&self, windows_11: bool) -> &str {
        self.get(
            "virtual_display_mode",
            if windows_11 { "per_client" } else { "disabled" },
        )
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
    /// Before 2.0.0 `amd_ltr_frames` defaulted to 0, and a console save
    /// could write that default into sunshine.conf, which kept AV1's
    /// recovery without a keyframe off after the upgrade. Until `upgraded`,
    /// a saved 0 is taken for that old default and dropped, so the current
    /// one applies; a 0 saved after the upgrade stays. Returns whether the
    /// saved value was dropped.
    pub fn upgrade_ltr_default(&mut self, upgraded: bool) -> bool {
        let old_default = !upgraded
            && self
                .values
                .get("amd_ltr_frames")
                .is_some_and(|value| parse_integer(value) == Some(0));
        if old_default {
            self.values.remove("amd_ltr_frames");
        }
        old_default
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
        // Only what changes is checked: a value the file already had (the
        // host reads it leniently) must not block every other save.
        let changed = |key: &str| object.contains_key(key) && next.values.contains_key(key);
        if changed("port") && !PORTS.contains(&next.integer("port", 0)) {
            bail!("port must be between 1029 and 65514");
        }
        if changed("frame_limiter_fps_limit") {
            crate::framegen::Rate::parse(next.get("frame_limiter_fps_limit", "0"))?;
        }
        *self = next;
        Ok(())
    }
    /// Before `keys` are written to sunshine.conf: values the file would not
    /// give back, and settings that would keep the host from starting.
    pub fn check_saved<'a>(&self, keys: impl IntoIterator<Item = &'a String>) -> Result<()> {
        for key in keys {
            let Some(value) = self.values.get(key) else {
                continue;
            };
            // '#' starts a comment and a value opening [ or { runs until it
            // closes, so such a value would be cut short, or swallow the
            // settings after it, on the next start.
            let expected = value.lines().map(str::trim).collect::<Vec<_>>().join("\n");
            let stored = Self::parse(&format!("{key} = {value}\n"))?;
            if stored.values.get(key).map(String::as_str) != Some(expected.trim()) {
                bail!(
                    "{key} cannot be saved: in the configuration file # starts a comment and a value opening [ or {{ has to close it"
                );
            }
            let value = unquote(value.trim());
            if key == "bind_address" && value.parse::<std::net::IpAddr>().is_err() {
                bail!("bind_address must be an IP address of this PC, or blank for all of them");
            }
            if key == "log_path"
                && (value.ends_with(['\\', '/'])
                    || (Path::new(value).is_absolute() && Path::new(value).is_dir()))
            {
                bail!("log_path must name a file, not a folder");
            }
        }
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
            // RTX HDR converts an SDR source for the HDR stream.
            hdr: if hdr && crate::rtx_policy::enabled(self) {
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
/// Whether a device or an app may override `key` for its streams. As in
/// Vibepollo, overrides cover stream, input, display and encoder behaviour;
/// identity, network, paths and integrations stay host-wide.
pub fn override_allowed(key: &str) -> bool {
    const ALLOWED: &[&str] = &[
        // Input
        "controller",
        "gamepad",
        "ds4_back_as_touchpad_click",
        "motion_as_ds4",
        "touchpad_as_ds4",
        "back_button_timeout",
        "back_grip_r4",
        "back_grip_l4",
        "back_grip_r5",
        "back_grip_l5",
        "steam_deck_controller",
        "keyboard",
        "key_repeat_delay",
        "key_repeat_frequency",
        "always_send_scancodes",
        "key_rightalt_to_key_win",
        "mouse",
        "high_resolution_scrolling",
        "native_pen_touch",
        "keybindings",
        // Audio, video and display automation
        "audio_sink",
        "audio_sink_capture_only",
        "virtual_sink",
        "stream_audio",
        "adapter_name",
        "adapter_pnp_id",
        "dd_configuration_option",
        "dd_resolution_option",
        "dd_manual_resolution",
        "dd_refresh_rate_option",
        "dd_manual_refresh_rate",
        "dd_hdr_option",
        "dd_hdr_request_override",
        "dd_config_revert_delay",
        "dd_config_revert_on_disconnect",
        "dd_paused_virtual_display_timeout_secs",
        "dd_always_restore_from_golden",
        "dd_snapshot_exclude_devices",
        "dd_snapshot_restore_hotkey",
        "dd_snapshot_restore_hotkey_modifiers",
        "dd_activate_virtual_display",
        "dd_virtual_display_scale",
        "dd_virtual_display_permanent_count",
        "dd_mode_remapping",
        "dd_wa_dummy_plug_hdr10",
        "max_bitrate",
        "minimum_fps_target",
        // Codec and capture
        "fec_percentage",
        "video_max_batch_size_kb",
        "pyrowave_critical_fec_percentage",
        "qp",
        "min_threads",
        "hevc_mode",
        "av1_mode",
        "capture",
        "encoder",
        // Playnite focus
        "playnite_focus_attempts",
        "playnite_focus_timeout_secs",
        "playnite_focus_exit_on_first",
        // Frame limiter
        "frame_limiter_enable",
        "frame_limiter_provider",
        "frame_limiter_fps_limit",
        "frame_limiter_auto_virtual_framegen",
        "rtss_frame_limit_type",
        "frame_limiter_disable_vsync",
        // Encoder tuning
        "nvenc_preset",
        "nvenc_twopass",
        "nvenc_spatial_aq",
        "nvenc_temporal_aq",
        "nvenc_split_encode",
        "nvenc_vbv_increase",
        "nvenc_realtime_hags",
        "nvenc_latency_over_power",
        "nvenc_opengl_vulkan_on_dxgi",
        "nvenc_h264_cavlc",
        "qsv_preset",
        "qsv_coder",
        "qsv_slow_hevc",
        "amd_usage",
        "amd_rc",
        "amd_peak_bitrate_ratio",
        "amd_vbv_buffer_frames",
        "amd_max_frame_size",
        "amd_qvbr_quality_level",
        "amd_enforce_hrd",
        "amd_quality",
        "amd_preanalysis",
        "amd_vbaq",
        "amd_coder",
        "amd_ltr_frames",
        "amd_input_queue_size",
        "amd_smart_access_video",
        "amd_split_frame",
        "amd_lowlatency_mode",
        "amd_high_motion_quality_boost",
        "amd_av1_screen_content",
        "amd_av1_latency_mode",
        "rtx_hdr",
        "rtx_hdr_sdr_brightness",
        "rtx_hdr_contrast",
        "rtx_hdr_saturation",
        "rtx_hdr_middle_gray",
        "rtx_hdr_peak_brightness",
        "sw_preset",
        "sw_tune",
    ];
    ALLOWED.contains(&key)
}
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
/// The text of sunshine.conf. Editors save it as UTF-8 with or without a
/// byte order mark, or as UTF-16 with one; anything else is read as the
/// Windows ANSI code page (Windows-1252) rather than refused.
fn decode(bytes: &[u8]) -> String {
    let utf16 = |bytes: &[u8], unit: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| unit(pair))
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(b"\xff\xfe") {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xfe\xff") {
        return utf16(rest, u16::from_be_bytes);
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    tracing::warn!("the configuration is not UTF-8; it is read as Windows-1252");
    // Windows-1252 differs from Latin-1 only in 0x80-0x9F; its five unused
    // codes map to the same control characters, as browsers read them.
    const HIGH: [char; 32] = [
        '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}',
        '\u{8f}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}',
        '\u{2014}', '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}',
        '\u{178}',
    ];
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9f => HIGH[usize::from(b - 0x80)],
            _ => char::from(b),
        })
        .collect()
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
    #[test]
    fn a_saved_ltr_zero_from_before_2_0_0_gives_way_to_the_new_default_once() {
        let mut config = super::Config::parse("amd_ltr_frames = 0\nport = 47989\n").unwrap();
        assert!(config.upgrade_ltr_default(false));
        assert!(!config.values.contains_key("amd_ltr_frames"));
        assert_eq!(config.integer("port", 0), 47989);
        // Chosen after the upgrade, 0 stays off.
        let mut config = super::Config::parse("amd_ltr_frames = 0\n").unwrap();
        assert!(!config.upgrade_ltr_default(true));
        assert_eq!(config.integer("amd_ltr_frames", 4), 0);
        // Other values are the user's own.
        for text in ["", "amd_ltr_frames = 1\n", "amd_ltr_frames = 4\n"] {
            let mut config = super::Config::parse(text).unwrap();
            let before = config.values.clone();
            assert!(!config.upgrade_ltr_default(false), "{text}");
            assert_eq!(config.values, before);
        }
    }
    #[test]
    fn overrides_allow_stream_input_display_and_encoder_keys_but_not_host_settings() {
        for key in [
            "max_bitrate",
            "stream_audio",
            "keyboard",
            "gamepad",
            "dd_hdr_option",
            "dd_mode_remapping",
            "encoder",
            "nvenc_preset",
            "amd_quality",
            "frame_limiter_fps_limit",
        ] {
            assert!(super::override_allowed(key), "{key}");
        }
        for key in [
            "port",
            "bind_address",
            "file_apps",
            "cert",
            "pkey",
            "enable_pairing",
            "server_cmd",
            "global_prep_cmd",
            "log_path",
            "unknown",
        ] {
            assert!(!super::override_allowed(key), "{key}");
        }
    }
    use super::*;
    /// A change saved to sunshine.conf, as the settings API does it.
    fn change(config: &mut Config, key: &str, value: &str) -> Result<()> {
        let mut object = serde_json::Map::new();
        object.insert(key.into(), value.into());
        let mut next = config.clone();
        next.update(&object)?;
        next.check_saved(object.keys())?;
        *config = next;
        Ok(())
    }
    #[test]
    fn overrides_that_are_never_saved_may_hold_a_comment_sign() {
        let mut config = Config::default();
        let mut object = serde_json::Map::new();
        object.insert("sunshine_name".into(), "Gaming PC #2".into());
        config.update(&object).unwrap();
        assert_eq!(config.get("sunshine_name", ""), "Gaming PC #2");
    }
    #[test]
    fn a_bad_value_already_in_the_file_does_not_block_other_saves() {
        let mut config = Config::parse("port = 70000\nframe_limiter_fps_limit = -1\n").unwrap();
        change(&mut config, "encoder", "amf").unwrap();
        assert_eq!(config.get("encoder", ""), "amf");
        assert!(change(&mut config, "port", "70000").is_err());
        change(&mut config, "port", "48000").unwrap();
    }
    #[test]
    fn values_the_file_cannot_give_back_are_refused() {
        let mut config = Config::default();
        // A comment would cut it short.
        assert!(change(&mut config, "sunshine_name", "Gaming PC #2").is_err());
        change(&mut config, "sunshine_name", "\"Gaming PC #2\"").unwrap();
        // An unclosed bracket would swallow the settings after it.
        assert!(change(&mut config, "adapter_name", "[Radeon").is_err());
        assert!(!config.values.contains_key("adapter_name"));
        // Spaces around a value and indented lists come back as the file reads them.
        change(&mut config, "sunshine_name", "  Living room  ").unwrap();
        change(&mut config, "global_prep_cmd", "[\n  {\"do\": \"a\"}\n]").unwrap();
        let reread = Config::parse(&config.text()).unwrap();
        assert_eq!(reread.get("sunshine_name", ""), "Living room");
        assert_eq!(reread.get("global_prep_cmd", ""), "[\n{\"do\": \"a\"}\n]");
    }
    #[test]
    fn bind_address_and_log_path_that_would_stop_the_host_are_refused() {
        let mut config = Config::default();
        assert!(change(&mut config, "bind_address", "Ethernet").is_err());
        change(&mut config, "bind_address", "192.168.1.5").unwrap();
        change(&mut config, "bind_address", "").unwrap();
        assert!(!config.values.contains_key("bind_address"));
        assert!(change(&mut config, "log_path", "C:\\Logs\\").is_err());
        let folder = std::env::temp_dir();
        assert!(change(&mut config, "log_path", folder.to_str().unwrap()).is_err());
        change(&mut config, "log_path", "D:\\Logs\\host.log").unwrap();
    }
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
    fn retired_vigem_controllers_become_their_vhf_pads() {
        let c = Config::parse("gamepad = x360\n").unwrap();
        assert_eq!(c.get("gamepad", ""), "vhf_xbox_one");
        assert_eq!(c.text().trim(), "gamepad = vhf_xbox_one");
        let c = Config::parse("gamepad=ds4\n").unwrap();
        assert_eq!(c.get("gamepad", ""), "vhf_ds4");
        for kept in ["auto", "vhf_ds5", "ds5", "vhf_xbox"] {
            let c = Config::parse(&format!("gamepad={kept}\n")).unwrap();
            assert_eq!(c.get("gamepad", ""), kept);
        }
        // Only the gamepad setting has these retired values.
        assert_eq!(
            Config::parse("unknown_key=x360\n")
                .unwrap()
                .get("unknown_key", ""),
            "x360"
        );
        assert_eq!(replacement("gamepad", "x360"), Some("vhf_xbox_one"));
        assert_eq!(replacement("gamepad", "vhf_ds4"), None);
        // An app's or a device's own settings.
        let mut app = serde_json::json!({"name": "x360", "gamepad": "ds4", "output": "x360"});
        assert!(replace_retired(app.as_object_mut().unwrap()));
        assert_eq!(
            app,
            serde_json::json!({"name": "x360", "gamepad": "vhf_ds4", "output": "x360"})
        );
        assert!(!replace_retired(app.as_object_mut().unwrap()));
        let mut app = serde_json::json!({"name": "Game", "gamepad": "x360",
            "config-overrides": {"gamepad": "ds4", "fec_percentage": "30"}});
        assert!(replace_retired_in_app(app.as_object_mut().unwrap()));
        assert_eq!(
            app,
            serde_json::json!({"name": "Game", "gamepad": "vhf_xbox_one",
                "config-overrides": {"gamepad": "vhf_ds4", "fec_percentage": "30"}})
        );
        assert!(!replace_retired_in_app(app.as_object_mut().unwrap()));
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
    fn files_saved_as_utf16_or_in_the_ansi_code_page_load() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("sunshine.conf");
        let text = "sunshine_name = Café € PC\r\nport = 48123 # base\r\n";
        let mut utf16 = vec![0xff, 0xfe];
        utf16.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        let mut big_endian = vec![0xfe, 0xff];
        big_endian.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
        let mut bom = b"\xef\xbb\xbf".to_vec();
        bom.extend_from_slice(text.as_bytes());
        let ansi = b"sunshine_name = Caf\xe9 \x80 PC\r\nport = 48123 # base\r\n".to_vec();
        for bytes in [utf16, big_endian, bom, ansi, text.as_bytes().to_vec()] {
            std::fs::write(&path, bytes).unwrap();
            let c = Config::load(&path).unwrap();
            assert_eq!(c.get("sunshine_name", ""), "Café € PC");
            assert_eq!(c.port().unwrap(), 48123);
            assert_eq!(c.values.len(), 2);
        }
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
