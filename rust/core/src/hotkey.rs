//! The display restore hotkey (`dd_snapshot_restore_hotkey` and
//! `dd_snapshot_restore_hotkey_modifiers`), read as Vibepollo reads them.
use crate::config::Config;

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

/// The key and modifiers to register, or `None` when the hotkey is off.
pub fn restore_hotkey(config: &Config) -> Option<(u32, u32)> {
    let key = key_code(config.get("dd_snapshot_restore_hotkey", ""));
    let modifiers = config
        .values
        .get("dd_snapshot_restore_hotkey_modifiers")
        .map_or(MOD_CONTROL | MOD_ALT | MOD_SHIFT, |value| modifiers(value));
    (key != 0).then_some((key, modifiers))
}
/// A virtual-key code: `F1`–`F24`, a letter or digit, `0x..` or a decimal
/// code, with an optional `VK_` prefix. 0 means off.
pub fn key_code(value: &str) -> u32 {
    let lower = value.trim().to_ascii_lowercase();
    // "0" is Vibepollo's stored default, so it means off rather than the 0 key.
    if matches!(lower.as_str(), "" | "0" | "disabled" | "none" | "off") {
        return 0;
    }
    let lower = lower.strip_prefix("vk_").unwrap_or(&lower);
    let valid = |code: u32| if (1..=0xff).contains(&code) { code } else { 0 };
    if let Some(hex) = lower.strip_prefix("0x") {
        return u32::from_str_radix(hex, 16).map_or(0, valid);
    }
    if let Some(number) = lower.strip_prefix('f')
        && let Ok(n @ 1..=24) = number.parse::<u32>()
    {
        return 0x70 + n - 1;
    }
    if let [c] = lower.as_bytes() {
        if c.is_ascii_lowercase() {
            return u32::from(c.to_ascii_uppercase());
        }
        if c.is_ascii_digit() {
            return u32::from(*c);
        }
    }
    lower.parse().map_or(0, valid)
}
/// `ctrl+alt+shift`, `win`, ... separated by spaces, `+`, `|`, `,` or `;`.
pub fn modifiers(value: &str) -> u32 {
    let lower = value.trim().to_ascii_lowercase();
    match lower.as_str() {
        "default" => return MOD_CONTROL | MOD_ALT | MOD_SHIFT,
        "" | "disabled" | "none" | "off" => return 0,
        _ => {}
    }
    lower
        .split(|c: char| c.is_whitespace() || matches!(c, '+' | '|' | ',' | ';'))
        .map(|token| match token {
            "ctrl" | "control" => MOD_CONTROL,
            "alt" => MOD_ALT,
            "shift" => MOD_SHIFT,
            "win" | "windows" | "meta" => MOD_WIN,
            _ => 0,
        })
        .fold(0, |all, modifier| all | modifier)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_and_modifiers_read_like_vibepollo() {
        assert_eq!(key_code("F12"), 0x7b);
        assert_eq!(key_code("vk_f1"), 0x70);
        assert_eq!(key_code("r"), u32::from(b'R'));
        assert_eq!(key_code("7"), u32::from(b'7'));
        assert_eq!(key_code("0x2E"), 0x2e);
        assert_eq!(key_code("46"), 46);
        for off in ["", "off", "None", "0", "0x100", "f25", "pause"] {
            assert_eq!(key_code(off), 0, "{off}");
        }
        assert_eq!(modifiers("Ctrl+Alt"), MOD_CONTROL | MOD_ALT);
        assert_eq!(modifiers("win; shift"), MOD_WIN | MOD_SHIFT);
        assert_eq!(modifiers("default"), MOD_CONTROL | MOD_ALT | MOD_SHIFT);
        assert_eq!(modifiers("none"), 0);
        let config = Config::parse("dd_snapshot_restore_hotkey=F9\n").unwrap();
        assert_eq!(
            restore_hotkey(&config),
            Some((0x78, MOD_CONTROL | MOD_ALT | MOD_SHIFT))
        );
        assert_eq!(restore_hotkey(&Config::default()), None);
    }
}
