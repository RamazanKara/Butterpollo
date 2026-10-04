//! Lossless Scaling for an app, as Vibepollo configures it: the app's
//! `lossless-scaling-*` fields become a Lossless Scaling game profile, and
//! frame generation lowers the stream's frame limit.
use crate::config::Config;
use serde_json::Value;

/// The profile values written into Lossless Scaling's settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub capture_api: Option<&'static str>,
    pub queue_target: Option<i64>,
    pub hdr: Option<bool>,
    /// `LSFG3` with frame generation, otherwise `Off`.
    pub frame_generation: &'static str,
    pub lsfg3_mode: Option<&'static str>,
    pub performance_mode: bool,
    pub flow_scale: i64,
    pub target_fps: Option<f64>,
    pub scale_factor: f64,
    pub scaling_type: &'static str,
    pub sharpness: Option<i64>,
    pub ls1_sharpness: Option<i64>,
    pub anime4k_type: Option<String>,
    pub anime4k_vrs: Option<bool>,
    /// Let Lossless Scaling start scaling by itself instead of the hotkey.
    pub auto_scale: bool,
}
/// Lossless Scaling for one app launch.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub profile: Profile,
    pub frame_generation: bool,
    /// The frame limit while Lossless Scaling generates frames.
    pub frame_limit: Option<u32>,
    /// Seconds to wait after the game starts.
    pub launch_delay: u64,
    pub legacy_auto_detect: bool,
}
fn flag(app: &Value, key: &str) -> Option<bool> {
    match app.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => Some(n.as_f64() != Some(0.)),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" | "" => Some(false),
            _ => None,
        },
        _ => None,
    }
}
fn number(app: &Value, key: &str) -> Option<f64> {
    match app.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}
/// The scaling modes Lossless Scaling knows, by the names apps use.
fn scaling_type(mode: &str) -> Option<&'static str> {
    Some(match mode {
        "off" => "Off",
        "ls1" => "LS1",
        "fsr" => "FSR",
        "nis" => "NIS",
        "sgsr" => "SGSR",
        "bcas" => "BicubicCAS",
        "anime4k" => "Anime4k",
        "xbr" => "XBR",
        "sharp-bilinear" => "SharpBilinear",
        "integer" => "Integer",
        "nearest" => "Nearest",
        _ => return None,
    })
}
/// The app's Lossless Scaling settings, or `None` when it does not use
/// Lossless Scaling. `client_fps` is the stream's frame rate.
pub fn options(app: &Value, config: &Config, client_fps: f64) -> Option<Options> {
    let framegen_flag = flag(app, "lossless-scaling-framegen").unwrap_or(false);
    let enabled = flag(app, "lossless-scaling-enabled").unwrap_or(framegen_flag);
    let mode = app
        .get("frame-generation-mode")
        .and_then(Value::as_str)
        .map(|m| m.trim().to_ascii_lowercase());
    let provider = |value: &str| {
        let value: String = value
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        matches!(value.as_str(), "losslessscaling" | "lossless" | "lsfg")
    };
    let frame_generation = match mode.as_deref() {
        Some("off" | "none" | "disabled") => false,
        Some(mode) => provider(mode),
        None => {
            framegen_flag
                && app
                    .get("frame-generation-provider")
                    .and_then(Value::as_str)
                    .is_none_or(provider)
        }
    };
    if !enabled && !frame_generation {
        return None;
    }
    let recommended = app
        .get("lossless-scaling-profile")
        .and_then(Value::as_str)
        .is_some_and(|p| p.eq_ignore_ascii_case("recommended"));
    let overrides = app
        .get(if recommended {
            "lossless-scaling-recommended"
        } else {
            "lossless-scaling-custom"
        })
        .cloned()
        .unwrap_or_default();
    let performance_mode = flag(&overrides, "performance-mode").unwrap_or(recommended);
    let flow_scale = number(&overrides, "flow-scale")
        .map_or(50, |v| v as i64)
        .clamp(0, 100);
    let mode = overrides
        .get("scaling-type")
        .and_then(Value::as_str)
        .map(|m| m.trim().to_ascii_lowercase())
        .filter(|m| scaling_type(m).is_some())
        .unwrap_or_else(|| "off".into());
    let scale_factor = if mode == "off" {
        1.0
    } else {
        let percent = number(&overrides, "resolution-scale")
            .map_or(100, |v| v as i64)
            .clamp(10, 100);
        ((100.0 / percent as f64).clamp(1.0, 10.0) * 100.0).round() / 100.0
    };
    let sharpness = matches!(mode.as_str(), "ls1" | "fsr" | "nis" | "sgsr").then(|| {
        number(&overrides, "sharpening")
            .map_or(5, |v| v as i64)
            .clamp(1, 10)
    });
    let anime = mode == "anime4k";
    let target_fps = frame_generation.then(|| {
        number(app, "lossless-scaling-target-fps")
            .filter(|v| *v > 0.)
            .unwrap_or(client_fps)
            .clamp(1., 480.)
    });
    let frame_limit = target_fps.map(|target| {
        number(app, "lossless-scaling-rtss-limit")
            .filter(|v| *v > 0.)
            .map_or((target * 0.5).round() as u32, |v| v as u32)
            .max(1)
    });
    let legacy_auto_detect = flag(app, "lossless-scaling-legacy-auto-detect")
        .unwrap_or_else(|| config.boolean("lossless_scaling_legacy_auto_detect", false));
    Some(Options {
        profile: Profile {
            capture_api: recommended.then_some("WGC"),
            queue_target: recommended.then_some(0),
            hdr: recommended.then_some(true),
            frame_generation: if frame_generation { "LSFG3" } else { "Off" },
            lsfg3_mode: (recommended && frame_generation).then_some("ADAPTIVE"),
            performance_mode,
            flow_scale,
            target_fps,
            scale_factor,
            scaling_type: scaling_type(&mode).unwrap_or("Off"),
            sharpness,
            ls1_sharpness: sharpness.filter(|_| mode == "ls1"),
            anime4k_type: anime.then(|| {
                overrides
                    .get("anime4k-size")
                    .and_then(Value::as_str)
                    .unwrap_or("S")
                    .to_ascii_uppercase()
            }),
            anime4k_vrs: anime.then(|| flag(&overrides, "anime4k-vrs").unwrap_or(false)),
            auto_scale: legacy_auto_detect,
        },
        frame_generation,
        frame_limit,
        launch_delay: number(app, "lossless-scaling-launch-delay").map_or(8, |v| v.max(0.) as u64),
        legacy_auto_detect,
    })
}
/// Lossless Scaling's hotkey (`Settings.Hotkey` and
/// `Settings.HotkeyModifierKeys`) as virtual-key codes: modifiers in press
/// order, then the key.
pub fn hotkey(key: &str, modifiers: &str) -> Option<(Vec<u16>, u16)> {
    let text = key.trim().to_ascii_uppercase();
    let code = match text.as_str() {
        single if single.len() == 1 && single.as_bytes()[0].is_ascii_alphanumeric() => {
            u16::from(single.as_bytes()[0])
        }
        "SPACE" => 0x20,
        "TAB" => 0x09,
        "ESC" | "ESCAPE" => 0x1b,
        "ENTER" | "RETURN" => 0x0d,
        "BACK" | "BACKSPACE" => 0x08,
        "INSERT" => 0x2d,
        "DELETE" => 0x2e,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PGUP" => 0x21,
        "PAGEDOWN" | "PGDN" => 0x22,
        "UP" => 0x26,
        "DOWN" => 0x28,
        "LEFT" => 0x25,
        "RIGHT" => 0x27,
        other => {
            if let Some(n) = other
                .strip_prefix('F')
                .and_then(|n| n.parse::<u16>().ok())
                .filter(|n| (1..=24).contains(n))
            {
                0x70 + n - 1
            } else {
                let digit = other
                    .strip_prefix("NUMPAD")
                    .and_then(|d| d.parse::<u16>().ok())
                    .filter(|d| *d <= 9 && other.len() == 7)?;
                0x60 + digit
            }
        }
    };
    let words: Vec<String> = modifiers
        .split(|c: char| c.is_whitespace() || matches!(c, '+' | ',' | ';' | '|'))
        .map(str::to_ascii_lowercase)
        .collect();
    let held = |names: &[&str]| words.iter().any(|w| names.contains(&w.as_str()));
    let order = [
        (&["control", "ctrl"][..], 0x11),
        (&["alt", "menu"][..], 0x12),
        (&["shift"][..], 0x10),
        (&["win", "windows", "logo"][..], 0x5b),
    ];
    Some((
        order
            .iter()
            .filter(|(names, _)| held(names))
            .map(|(_, vk)| *vk)
            .collect(),
        code,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn app_fields_become_a_profile_like_vibepollo() {
        let config = Config::default();
        assert_eq!(options(&json!({"name": "Game"}), &config, 60.), None);
        let fg = options(&json!({"frame-generation-mode": "lossless-scaling", "lossless-scaling-profile": "recommended"}), &config, 120.).unwrap();
        assert!(fg.frame_generation);
        assert_eq!(fg.profile.frame_generation, "LSFG3");
        assert_eq!(fg.profile.lsfg3_mode, Some("ADAPTIVE"));
        assert_eq!(fg.profile.capture_api, Some("WGC"));
        assert!(fg.profile.performance_mode);
        assert_eq!(fg.profile.target_fps, Some(120.));
        assert_eq!(fg.frame_limit, Some(60));
        assert_eq!(fg.launch_delay, 8);
        let scaled = options(
            &json!({"lossless-scaling-enabled": "true", "lossless-scaling-custom": {"scaling-type": "FSR", "resolution-scale": 75, "sharpening": 12}, "lossless-scaling-launch-delay": 3}),
            &config,
            60.,
        )
        .unwrap();
        assert!(!scaled.frame_generation);
        assert_eq!(scaled.frame_limit, None);
        assert_eq!(scaled.profile.frame_generation, "Off");
        assert_eq!(scaled.profile.scaling_type, "FSR");
        assert_eq!(scaled.profile.scale_factor, 1.33);
        assert_eq!(scaled.profile.sharpness, Some(10));
        assert_eq!(scaled.profile.capture_api, None);
        assert_eq!(scaled.launch_delay, 3);
        let off = options(
            &json!({"lossless-scaling-framegen": true, "frame-generation-mode": "off"}),
            &config,
            60.,
        )
        .unwrap();
        assert!(!off.frame_generation);
        let legacy = Config::parse("lossless_scaling_legacy_auto_detect = true").unwrap();
        let limited = options(
            &json!({"lossless-scaling-framegen": true, "lossless-scaling-rtss-limit": 45}),
            &legacy,
            60.,
        )
        .unwrap();
        assert_eq!(limited.frame_limit, Some(45));
        assert!(limited.profile.auto_scale);
    }
    #[test]
    fn hotkeys_read_like_vibepollo() {
        assert_eq!(
            hotkey("S", "Alt, Ctrl"),
            Some((vec![0x11, 0x12], u16::from(b'S')))
        );
        assert_eq!(hotkey("F12", ""), Some((vec![], 0x7b)));
        assert_eq!(
            hotkey("numpad5", "shift+win"),
            Some((vec![0x10, 0x5b], 0x65))
        );
        assert_eq!(hotkey("pause", ""), None);
    }
}
