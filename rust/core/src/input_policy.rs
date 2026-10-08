//! How decoded input is applied: gamepad backend and profile selection,
//! per-session input permissions and the back-button mapping.
use crate::{config::Config, input::Input};
use anyhow::Result;
use std::{collections::BTreeMap, time::Duration};

// Host profile IDs; 3..=7 retain the VHF driver's profile numbers.
pub const VIGEM_X360: u16 = 8;
pub const VIGEM_DS4: u16 = 9;
pub const VHF_AUTO: u16 = 10;
pub const VIGEM_PROFILES: u32 = (1 << (VIGEM_X360 - 1)) | (1 << (VIGEM_DS4 - 1));
/// The VHF driver's DualSense, the only virtual pad with adaptive triggers.
pub const VHF_DUALSENSE: u16 = 6;

#[derive(Debug, PartialEq, Eq)]
pub enum GamepadBackend {
    Vigem,
    Vhf,
    /// Automatic with ViGEmBus installed: it keeps the Xbox-type pads, and the
    /// VHF driver is opened too, when installed, for PlayStation-type clients.
    Mixed,
}

pub fn gamepad_backend(profile: u16, vigem_available: bool) -> GamepadBackend {
    if matches!(profile, VIGEM_X360 | VIGEM_DS4) {
        GamepadBackend::Vigem
    } else if profile == 0 && vigem_available {
        GamepadBackend::Mixed
    } else {
        GamepadBackend::Vhf
    }
}

pub fn gamepad_profile(value: &str) -> u16 {
    match value {
        "x360" => VIGEM_X360,
        "ds4" => VIGEM_DS4,
        "vhf_xbox_one" => 3,
        "vhf_xbox" => 4,
        "vhf_ds4" => 5,
        "vhf_ds5" | "ds5" => 6,
        "vhf_switch" => 7,
        "vhf" => VHF_AUTO,
        other => {
            crate::config::fallback("gamepad", other, "auto");
            0
        }
    }
}

#[derive(Clone)]
pub struct Policy {
    pub keyboard: bool,
    pub mouse: bool,
    pub controller: bool,
    pub native_pen_touch: bool,
    pub always_send_scancodes: bool,
    pub high_resolution_scrolling: bool,
    pub forward_rumble: bool,
    pub motion_as_ds4: bool,
    pub touchpad_as_ds4: bool,
    /// None: the host does not repeat held keys.
    pub repeat_delay: Option<Duration>,
    pub repeat_period: Duration,
    pub back_button_timeout: Option<Duration>,
    pub keybindings: BTreeMap<u16, u16>,
}
impl Policy {
    pub fn resolve(config: &Config) -> Result<Self> {
        let mut keybindings = BTreeMap::from([(0x10, 0xa0), (0x11, 0xa2), (0x12, 0xa4)]);
        // Pairs of key codes, decimal or hexadecimal as Vibepollo writes them.
        // Unusable mappings keep the defaults instead of disabling input.
        let codes: Option<Vec<u16>> = config
            .list("keybindings")
            .iter()
            .map(|code| crate::config::parse_integer(code).and_then(|n| u16::try_from(n).ok()))
            .collect();
        match codes {
            Some(codes) if codes.len().is_multiple_of(2) => {
                for pair in codes.as_chunks::<2>().0 {
                    keybindings.insert(pair[0], pair[1]);
                }
            }
            _ => crate::config::invalid("keybindings", config.get("keybindings", "")),
        }
        if config.boolean("key_rightalt_to_key_win", false) {
            keybindings.entry(0xa5).or_insert(0x5b);
        }
        let frequency = config
            .get("key_repeat_frequency", "24.9")
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite() && (0.0..=1000.0).contains(f))
            .unwrap_or_else(|| {
                crate::config::invalid(
                    "key_repeat_frequency",
                    config.get("key_repeat_frequency", ""),
                );
                24.9
            });
        Ok(Self {
            keyboard: config.boolean("keyboard", true),
            mouse: config.boolean("mouse", true),
            controller: config.boolean("controller", true),
            native_pen_touch: config.boolean("native_pen_touch", true),
            always_send_scancodes: config.boolean("always_send_scancodes", true),
            high_resolution_scrolling: config.boolean("high_resolution_scrolling", true),
            forward_rumble: config.boolean("forward_rumble", true),
            motion_as_ds4: config.boolean("motion_as_ds4", true),
            touchpad_as_ds4: config.boolean("touchpad_as_ds4", true),
            // As in Vibepollo: 0 turns host key repeat off and a negative
            // value keeps the default. Clamping 0 to an immediate repeat
            // typed two or three characters per tap for imported profiles
            // that had turned repeat off.
            repeat_delay: match config.integer("key_repeat_delay", 500) {
                0 => None,
                ms if ms < 0 => Some(Duration::from_millis(500)),
                ms => Some(Duration::from_millis(ms.min(60000) as u64)),
            },
            repeat_period: Duration::from_secs_f64(
                1.0 / if frequency == 0.0 { 24.9 } else { frequency },
            ),
            back_button_timeout: u64::try_from(config.integer("back_button_timeout", -1))
                .ok()
                .map(|ms| Duration::from_millis(ms.min(60000))),
            keybindings,
        })
    }
    pub fn allows(&self, event: &Input) -> bool {
        match event {
            Input::Keyboard { .. } | Input::Text(_) => self.keyboard,
            Input::Relative { .. }
            | Input::Absolute { .. }
            | Input::MouseButton { .. }
            | Input::Scroll { .. } => self.mouse,
            Input::Pen { .. } | Input::Touch { .. } => self.mouse && self.native_pen_touch,
            _ => self.controller,
        }
    }
    pub fn key(&self, key: u16) -> u16 {
        self.keybindings.get(&key).copied().unwrap_or(key)
    }
    pub fn controller_profile(
        &self,
        configured: u16,
        kind: u8,
        capabilities: u16,
        available: u32,
    ) -> Option<u16> {
        if available & VIGEM_PROFILES != 0 {
            // ViGEm cannot emulate a DualSense, so a PlayStation-type client
            // gets the VHF one when that driver is installed too: it alone
            // carries adaptive triggers. Without it, the client falls back to
            // a ViGEm DualShock 4. Xbox-type clients stay on ViGEm, and an
            // explicit profile is never replaced.
            if configured == 0 && kind == 2 && available & (1 << (VHF_DUALSENSE - 1)) != 0 {
                return Some(VHF_DUALSENSE);
            }
            let desired = if configured != 0 {
                configured
            } else if kind == 2
                || (self.motion_as_ds4 && capabilities & 0x30 != 0)
                || (self.touchpad_as_ds4 && capabilities & 8 != 0)
            {
                VIGEM_DS4
            } else {
                VIGEM_X360
            };
            return (available & (1 << (desired - 1)) != 0).then_some(desired);
        }
        let desired = if configured != 0 {
            configured
        } else if kind == 2 {
            6
        } else if kind == 3 {
            7
        } else if (self.motion_as_ds4 && capabilities & 0x30 != 0)
            || (self.touchpad_as_ds4 && capabilities & 8 != 0)
        {
            // As in Vibepollo, a controller of any other type with motion
            // sensors or a touchpad becomes a DualSense, an Xbox-type one too
            // (a Steam Deck), so its gyro and touchpad keep working.
            6
        } else {
            0
        };
        [desired, 4, 6, 5]
            .into_iter()
            .find(|p| *p != 0 && available & (1 << (p - 1)) != 0)
    }
}

/// Back-to-Home emulation, advanced by the input worker without blocking it.
#[derive(Default)]
pub struct BackButton {
    pressed: Option<Duration>,
    home_until: Option<Duration>,
    suppressed: bool,
    physical: u32,
    output: u32,
}
impl BackButton {
    pub fn update(&mut self, buttons: u32, now: Duration, timeout: Option<Duration>) -> u32 {
        self.physical = buttons;
        if buttons & 0x20 == 0 {
            self.pressed = None;
            self.suppressed = false;
        } else if !self.suppressed && self.pressed.is_none() {
            self.pressed = Some(now);
        }
        self.advance(now, timeout);
        self.output
    }
    fn advance(&mut self, now: Duration, timeout: Option<Duration>) {
        if !self.suppressed
            && self
                .pressed
                .zip(timeout)
                .is_some_and(|(start, delay)| now.saturating_sub(start) >= delay)
        {
            self.suppressed = true;
            self.pressed = None;
            self.home_until = Some(now + Duration::from_millis(100));
        }
        if self.home_until.is_some_and(|until| now >= until) {
            self.home_until = None;
        }
        self.output = self.physical & if self.suppressed { !0x20 } else { u32::MAX };
        if self.home_until.is_some() {
            self.output |= 0x400;
        }
    }
    pub fn poll(&mut self, now: Duration, timeout: Option<Duration>) -> Option<u32> {
        let before = self.output;
        self.advance(now, timeout);
        (before != self.output).then_some(self.output)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_backend_adds_vhf_to_vigem_and_explicit_profiles_keep_their_backend() {
        use GamepadBackend::*;
        assert_eq!(gamepad_backend(gamepad_profile("auto"), true), Mixed);
        assert_eq!(gamepad_backend(gamepad_profile("auto"), false), Vhf);
        for available in [false, true] {
            for value in ["x360", "ds4"] {
                assert_eq!(gamepad_backend(gamepad_profile(value), available), Vigem);
            }
            for value in [
                "vhf",
                "vhf_xbox",
                "vhf_xbox_one",
                "vhf_ds4",
                "vhf_ds5",
                "vhf_switch",
            ] {
                assert_eq!(gamepad_backend(gamepad_profile(value), available), Vhf);
            }
        }
    }
    #[test]
    fn vigem_selection_preserves_motion_and_touch_for_all_client_types() {
        let policy = Policy::resolve(&Config::default()).unwrap();
        for kind in 0..=3 {
            for caps in [8, 0x10, 0x20, 0x38] {
                assert_eq!(
                    policy.controller_profile(0, kind, caps, VIGEM_PROFILES),
                    Some(VIGEM_DS4)
                );
                assert_eq!(
                    policy.controller_profile(VIGEM_X360, kind, caps, VIGEM_PROFILES),
                    Some(VIGEM_X360)
                );
            }
            assert_eq!(
                policy.controller_profile(0, kind, 0, VIGEM_PROFILES),
                Some(if kind == 2 { VIGEM_DS4 } else { VIGEM_X360 })
            );
            assert_eq!(
                policy.controller_profile(VIGEM_DS4, kind, 0, VIGEM_PROFILES),
                Some(VIGEM_DS4)
            );
        }
        for (settings, motion, touch) in [
            ("motion_as_ds4=false", VIGEM_X360, VIGEM_DS4),
            ("touchpad_as_ds4=false", VIGEM_DS4, VIGEM_X360),
            (
                "motion_as_ds4=false\ntouchpad_as_ds4=false",
                VIGEM_X360,
                VIGEM_X360,
            ),
        ] {
            let policy = Policy::resolve(&Config::parse(settings).unwrap()).unwrap();
            assert_eq!(
                policy.controller_profile(0, 1, 0x30, VIGEM_PROFILES),
                Some(motion)
            );
            assert_eq!(
                policy.controller_profile(0, 1, 8, VIGEM_PROFILES),
                Some(touch)
            );
            assert_eq!(
                policy.controller_profile(0, 2, 0x38, VIGEM_PROFILES),
                Some(VIGEM_DS4)
            );
        }
        assert_eq!(policy.controller_profile(0, 1, 0, 0), None);
    }
    #[test]
    fn playstation_clients_prefer_vhf_dualsense_when_both_drivers_are_installed() {
        let policy = Policy::resolve(&Config::default()).unwrap();
        // The VHF driver's Xbox One, Xbox Series, DualShock 4, DualSense and
        // Switch Pro profiles next to ViGEmBus's two.
        let both = VIGEM_PROFILES | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6);
        for caps in [0, 8, 0x10, 0x20, 0x38, 0xff] {
            assert_eq!(
                policy.controller_profile(0, 2, caps, both),
                Some(VHF_DUALSENSE)
            );
            // An explicit ViGEm choice is not replaced.
            assert_eq!(
                policy.controller_profile(VIGEM_DS4, 2, caps, both),
                Some(VIGEM_DS4)
            );
            assert_eq!(
                policy.controller_profile(VIGEM_X360, 2, caps, both),
                Some(VIGEM_X360)
            );
        }
        // Everyone else is exactly what ViGEmBus alone gives them: a Steam
        // Deck reporting motion keeps its DualShock 4.
        for kind in [0, 1, 3, 4] {
            for caps in [0, 8, 0x10, 0x20, 0x38] {
                assert_eq!(
                    policy.controller_profile(0, kind, caps, both),
                    policy.controller_profile(0, kind, caps, VIGEM_PROFILES)
                );
            }
        }
        assert_eq!(policy.controller_profile(0, 1, 0, both), Some(VIGEM_X360));
        assert_eq!(policy.controller_profile(0, 1, 0x30, both), Some(VIGEM_DS4));
        // The motion and touchpad preferences only choose for Xbox-type clients.
        let plain = Policy::resolve(
            &Config::parse("motion_as_ds4=false\ntouchpad_as_ds4=false\n").unwrap(),
        )
        .unwrap();
        assert_eq!(plain.controller_profile(0, 2, 0, both), Some(VHF_DUALSENSE));
        assert_eq!(plain.controller_profile(0, 1, 0x38, both), Some(VIGEM_X360));
    }
    #[test]
    fn playstation_clients_fall_back_to_vigem_dualshock_without_a_vhf_dualsense() {
        let policy = Policy::resolve(&Config::default()).unwrap();
        // No VHF driver at all, as before.
        assert_eq!(
            policy.controller_profile(0, 2, 0x38, VIGEM_PROFILES),
            Some(VIGEM_DS4)
        );
        // A VHF driver without a DualSense profile.
        let no_dualsense = VIGEM_PROFILES | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 6);
        assert_eq!(
            policy.controller_profile(0, 2, 0x38, no_dualsense),
            Some(VIGEM_DS4)
        );
        // The VHF driver alone keeps its own choice.
        let vhf = (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6);
        assert_eq!(policy.controller_profile(0, 2, 0, vhf), Some(VHF_DUALSENSE));
        assert_eq!(policy.controller_profile(0, 1, 0, vhf), Some(4));
    }
    #[test]
    fn independent_controllers_match_client_type_and_available_driver_profiles() {
        let policy = Policy::resolve(&Config::default()).unwrap();
        let all = (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6);
        assert_eq!(policy.controller_profile(0, 1, 0x4, all), Some(4));
        assert_eq!(policy.controller_profile(0, 2, 0, all), Some(6));
        assert_eq!(policy.controller_profile(0, 3, 0x30, all), Some(7));
        assert_eq!(policy.controller_profile(0, 0, 8, all), Some(6));
        assert_eq!(policy.controller_profile(5, 3, 0, all), Some(5));
        assert_eq!(policy.controller_profile(0, 2, 0, 1 << 3), Some(4));
        // An Xbox-type controller with motion or a touchpad (a Steam Deck), as
        // in Vibepollo; without either it stays an Xbox pad.
        assert_eq!(policy.controller_profile(0, 1, 0x30 | 8, all), Some(6));
        assert_eq!(policy.controller_profile(0, 1, 0x30, all), Some(6));
        let plain = Policy::resolve(
            &Config::parse("motion_as_ds4=false\ntouchpad_as_ds4=false\n").unwrap(),
        )
        .unwrap();
        assert_eq!(plain.controller_profile(0, 1, 0x30 | 8, all), Some(4));
    }
    #[test]
    fn key_repeat_delay_follows_vibepollo() {
        let delay = |value: &str| {
            let config = Config::parse(&format!("key_repeat_delay={value}\n")).unwrap();
            Policy::resolve(&config).unwrap().repeat_delay
        };
        assert_eq!(delay("0"), None);
        assert_eq!(delay("-5"), Some(Duration::from_millis(500)));
        assert_eq!(delay("250"), Some(Duration::from_millis(250)));
        assert_eq!(delay("999999"), Some(Duration::from_millis(60000)));
        assert_eq!(
            Policy::resolve(&Config::default()).unwrap().repeat_delay,
            Some(Duration::from_millis(500))
        );
    }
    #[test]
    fn held_back_emits_one_home_pulse_and_requires_release_before_rearming() {
        let mut button = BackButton::default();
        let delay = Some(Duration::from_millis(500));
        let at = Duration::from_millis;
        assert_eq!(button.update(0x20 | 0x1000, at(0), delay), 0x1020);
        assert_eq!(button.poll(at(499), delay), None);
        assert_eq!(button.poll(at(500), delay), Some(0x1400));
        assert_eq!(button.update(0x20 | 0x2000, at(550), delay), 0x2400);
        assert_eq!(button.poll(at(600), delay), Some(0x2000));
        assert_eq!(button.poll(at(5000), delay), None);
        assert_eq!(button.update(0, at(5001), delay), 0);
        assert_eq!(button.update(0x20, at(5002), None), 0x20);
        assert_eq!(button.poll(at(8000), None), None);
    }
    #[test]
    fn disabled_devices_and_explicit_right_alt_mapping_keep_legacy_precedence() {
        let p=Policy::resolve(&Config::parse("keyboard=disabled\nmouse=enabled\ncontroller=false\nkeybindings=[165,166]\nkey_rightalt_to_key_win=true\n").unwrap()).unwrap();
        assert!(!p.allows(&Input::Text("a".into())));
        assert!(p.allows(&Input::Relative { x: 2, y: -3 }));
        assert!(!p.allows(&Input::Arrival {
            id: 0,
            kind: 0,
            capabilities: 0,
            buttons: 0
        }));
        assert_eq!(p.key(0xa5), 0xa6);
        assert_eq!(p.key(0x11), 0xa2);
    }
}
