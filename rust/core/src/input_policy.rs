//! How decoded input is applied: gamepad backend and profile selection,
//! per-session input permissions and the back-button mapping.
use crate::{config::Config, input::Input};
use anyhow::Result;
use std::{collections::BTreeMap, time::Duration};

// Host profile IDs; 3..=7 are the VHF driver's profile numbers.
pub const VHF_AUTO: u16 = 10;
pub const VHF_XBOX_ONE: u16 = 3;
pub const VHF_XBOX_SERIES: u16 = 4;
pub const VHF_DS4: u16 = 5;
/// The VHF driver's DualSense, the only virtual pad with adaptive triggers.
pub const VHF_DUALSENSE: u16 = 6;
pub const VHF_SWITCH: u16 = 7;
/// Moonlight's `LI_CTYPE_STEAM`: a Valve controller such as the Steam Deck,
/// with an Xbox layout, two trackpads, motion sensors and back grips.
pub const CTYPE_STEAM: u8 = 4;

/// Moonlight's paddle buttons in the order of [`BACK_GRIP_KEYS`]: upper
/// right, upper left, lower right and lower left. They are a Steam Deck's
/// R4, L4, R5 and L5, and an Xbox Elite's P1, P3, P2 and P4.
pub const PADDLES: [u32; 4] = [0x01_0000, 0x02_0000, 0x04_0000, 0x08_0000];
/// The settings that choose what each back grip presses.
pub const BACK_GRIP_KEYS: [&str; 4] = [
    "back_grip_r4",
    "back_grip_l4",
    "back_grip_r5",
    "back_grip_l5",
];

/// The stream-card hint for a back grip pressed while it is unmapped.
pub const BACK_GRIP_HINT: &str = "A controller's back grips (a Steam Deck's L4, R4, L5 or R5) were pressed, but no virtual controller has them, so they do nothing. Choose what each one presses under Settings, Input, Controllers.";

/// The `steam_deck_controller` setting: what a Steam Deck client's
/// controller becomes on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeckPad {
    /// A real Steam Deck controller when usbip-win2 is installed and Steam
    /// is running on the host; otherwise the virtual pad.
    Auto,
    /// A real Steam Deck controller whenever usbip-win2 is installed.
    SteamDeck,
    /// Always the VHF pad, with back grips mapped by the `back_grip_*` settings.
    VirtualPad,
}
pub fn deck_pad(value: &str) -> DeckPad {
    match value {
        "auto" => DeckPad::Auto,
        "steam_deck" => DeckPad::SteamDeck,
        "virtual_pad" => DeckPad::VirtualPad,
        other => {
            crate::config::fallback("steam_deck_controller", other, "auto");
            DeckPad::Auto
        }
    }
}

/// What a back grip presses on the virtual pad, none of which has back grips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GripTarget {
    /// Moonlight button flags.
    Buttons(u32),
    LeftTrigger,
    RightTrigger,
}
/// The `back_grip_*` setting `key`; `none` and unknown values leave the grip
/// unmapped.
pub fn grip_target(key: &str, value: &str) -> Option<GripTarget> {
    use GripTarget::*;
    Some(match value {
        "a" => Buttons(0x1000),
        "b" => Buttons(0x2000),
        "x" => Buttons(0x4000),
        "y" => Buttons(0x8000),
        "lb" => Buttons(0x0100),
        "rb" => Buttons(0x0200),
        "lt" => LeftTrigger,
        "rt" => RightTrigger,
        "l3" => Buttons(0x0040),
        "r3" => Buttons(0x0080),
        "back" => Buttons(0x0020),
        "start" => Buttons(0x0010),
        "guide" => Buttons(0x0400),
        "dpad_up" => Buttons(0x0001),
        "dpad_down" => Buttons(0x0002),
        "dpad_left" => Buttons(0x0004),
        "dpad_right" => Buttons(0x0008),
        "touchpad" => Buttons(0x10_0000),
        "misc" => Buttons(0x20_0000),
        other => {
            crate::config::fallback(key, other, "none");
            return None;
        }
    })
}

/// The stream-card hint for a Steam Deck whose controller reaches Moonlight
/// through Steam Input: Moonlight then sees Steam's virtual pad, without the
/// gyro, trackpads and back grips, and so does the host. SDL may still name
/// it a Steam Deck (Steam tells it the real controller), so only the
/// missing sensors and the device's name give such a Deck away.
pub fn steam_input_hint(device: &str, capabilities: u16) -> Option<String> {
    let name: String = device
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (name.contains("steamdeck") && capabilities & 0x30 == 0).then(|| {
        format!(
            "{device} looks like a Steam Deck whose controls reach Moonlight through Steam Input, so the host gets its buttons and sticks but no gyro, trackpads or back grips. To pass them on, set Moonlight's controller settings in Steam on the Deck to disable Steam Input, then reconnect."
        )
    })
}

/// The `gamepad` setting's profile. The retired ViGEmBus choices `x360` and
/// `ds4`, which Sunshine, Apollo and Vibepollo profiles can carry, select the
/// VHF pad of the same family (see [`crate::config::RETIRED_VALUES`]).
pub fn gamepad_profile(value: &str) -> u16 {
    match value {
        "vhf_xbox_one" | "x360" => VHF_XBOX_ONE,
        "vhf_xbox" => VHF_XBOX_SERIES,
        "vhf_ds4" | "ds4" => VHF_DS4,
        "vhf_ds5" | "ds5" => VHF_DUALSENSE,
        "vhf_switch" => VHF_SWITCH,
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
    /// What each back grip presses, in [`PADDLES`] order.
    pub back_grips: [Option<GripTarget>; 4],
    pub steam_deck: DeckPad,
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
            back_grips: BACK_GRIP_KEYS.map(|key| grip_target(key, config.get(key, "none"))),
            steam_deck: deck_pad(config.get("steam_deck_controller", "auto")),
            keybindings,
        })
    }
    /// Presses what each mapped back grip is set to press, in place of the
    /// grip. An unmapped grip stays as it came: the driver ignores it.
    pub fn map_back_grips(&self, buttons: u32, left: u8, right: u8) -> (u32, u8, u8) {
        let (mut buttons, mut left, mut right) = (buttons, left, right);
        let held = buttons;
        for (flag, target) in PADDLES.into_iter().zip(self.back_grips) {
            let Some(target) = target else { continue };
            buttons &= !flag;
            if held & flag != 0 {
                match target {
                    GripTarget::Buttons(pressed) => buttons |= pressed,
                    GripTarget::LeftTrigger => left = u8::MAX,
                    GripTarget::RightTrigger => right = u8::MAX,
                }
            }
        }
        (buttons, left, right)
    }
    /// The back grips no setting maps, as Moonlight button flags.
    pub fn unmapped_back_grips(&self) -> u32 {
        PADDLES
            .into_iter()
            .zip(self.back_grips)
            .filter(|(_, target)| target.is_none())
            .fold(0, |mask, (flag, _)| mask | flag)
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
        let desired = if configured != 0 {
            configured
        } else if kind == 2 {
            VHF_DUALSENSE
        } else if kind == 3 {
            VHF_SWITCH
        } else if (self.motion_as_ds4 && capabilities & 0x30 != 0)
            || (self.touchpad_as_ds4 && capabilities & 8 != 0)
        {
            // As in Vibepollo, a controller of any other type with motion
            // sensors or a touchpad becomes a DualSense, an Xbox-type one too
            // (a Steam Deck), so its gyro and touchpad keep working.
            VHF_DUALSENSE
        } else {
            0
        };
        [desired, VHF_XBOX_SERIES, VHF_DUALSENSE, VHF_DS4]
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
    fn retired_vigem_choices_select_the_vhf_pad_of_their_family() {
        assert_eq!(gamepad_profile("x360"), VHF_XBOX_ONE);
        assert_eq!(gamepad_profile("ds4"), VHF_DS4);
        assert_eq!(gamepad_profile("vhf_xbox_one"), VHF_XBOX_ONE);
        assert_eq!(gamepad_profile("vhf_ds4"), VHF_DS4);
        assert_eq!(gamepad_profile("auto"), 0);
        let policy = Policy::resolve(&Config::default()).unwrap();
        let vhf = (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6);
        // Whatever the client is, including an Android phone with no motion
        // or touchpad and one that sends no arrival at all.
        for kind in 0..=4 {
            for caps in [0, 8, 0x30, 0x38] {
                assert_eq!(
                    policy.controller_profile(gamepad_profile("x360"), kind, caps, vhf),
                    Some(VHF_XBOX_ONE)
                );
                assert_eq!(
                    policy.controller_profile(gamepad_profile("ds4"), kind, caps, vhf),
                    Some(VHF_DS4)
                );
            }
        }
        // A driver without the Xbox One profile still gives an Xbox pad.
        assert_eq!(
            policy.controller_profile(gamepad_profile("x360"), 1, 0, 1 << 3),
            Some(VHF_XBOX_SERIES)
        );
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
    fn a_steam_deck_with_its_sensors_gets_a_dualsense_and_without_them_an_xbox_pad() {
        let policy = Policy::resolve(&Config::default()).unwrap();
        let vhf = (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6);
        // What Moonlight announces for a Deck with Steam Input off: gyro and
        // accelerometer, and with SDL 3 both trackpads.
        for caps in [0x30, 0x38, 0x138] {
            assert_eq!(
                policy.controller_profile(0, CTYPE_STEAM, caps, vhf),
                Some(VHF_DUALSENSE)
            );
        }
        assert_eq!(policy.controller_profile(0, CTYPE_STEAM, 0, vhf), Some(4));
        let plain = Policy::resolve(
            &Config::parse("motion_as_ds4=false\ntouchpad_as_ds4=false\n").unwrap(),
        )
        .unwrap();
        assert_eq!(
            plain.controller_profile(0, CTYPE_STEAM, 0x138, vhf),
            Some(4)
        );
        // An explicit profile is kept.
        assert_eq!(
            policy.controller_profile(VHF_DS4, CTYPE_STEAM, 0x138, vhf),
            Some(VHF_DS4)
        );
    }
    #[test]
    fn back_grips_pass_through_until_mapped() {
        let all = PADDLES.iter().fold(0, |mask, flag| mask | flag);
        let policy = Policy::resolve(&Config::default()).unwrap();
        assert_eq!(policy.back_grips, [None; 4]);
        assert_eq!(policy.unmapped_back_grips(), all);
        assert_eq!(
            policy.map_back_grips(all | 0x1000, 7, 9),
            (all | 0x1000, 7, 9)
        );
    }
    #[test]
    fn mapped_back_grips_press_their_button_or_trigger_instead() {
        let policy = Policy::resolve(
            &Config::parse(
                "back_grip_r4=a\nback_grip_l4=lt\nback_grip_r5=rt\nback_grip_l5=nonsense\n",
            )
            .unwrap(),
        )
        .unwrap();
        let [r4, l4, r5, l5] = PADDLES;
        assert_eq!(
            policy.back_grips,
            [
                Some(GripTarget::Buttons(0x1000)),
                Some(GripTarget::LeftTrigger),
                Some(GripTarget::RightTrigger),
                None
            ]
        );
        assert_eq!(policy.unmapped_back_grips(), l5);
        // R4 presses A; held with A itself, A stays down.
        assert_eq!(policy.map_back_grips(r4, 0, 0), (0x1000, 0, 0));
        assert_eq!(policy.map_back_grips(r4 | 0x1000, 0, 0), (0x1000, 0, 0));
        // The trigger grips pull their trigger all the way, whatever it was at.
        assert_eq!(policy.map_back_grips(l4 | 0x10, 40, 50), (0x10, 255, 50));
        assert_eq!(policy.map_back_grips(r5, 40, 50), (0, 40, 255));
        // Released grips press nothing; the unmapped one passes through.
        assert_eq!(policy.map_back_grips(0x2000, 40, 50), (0x2000, 40, 50));
        assert_eq!(policy.map_back_grips(l5, 0, 0), (l5, 0, 0));
        // Every value the console offers maps to something.
        for value in [
            "a",
            "b",
            "x",
            "y",
            "lb",
            "rb",
            "lt",
            "rt",
            "l3",
            "r3",
            "back",
            "start",
            "guide",
            "dpad_up",
            "dpad_down",
            "dpad_left",
            "dpad_right",
            "touchpad",
            "misc",
        ] {
            assert!(grip_target("back_grip_r4", value).is_some(), "{value}");
        }
        assert_eq!(grip_target("back_grip_r4", "none"), None);
    }
    #[test]
    fn the_steam_input_hint_names_a_deck_whose_sensors_did_not_arrive() {
        for name in ["steamdeck", "Steam Deck", "steam-deck (OLED)"] {
            let hint = steam_input_hint(name, 0x47).unwrap();
            assert!(hint.starts_with(name));
            assert!(hint.contains("disable Steam Input"));
            assert!(steam_input_hint(name, 0x03).is_some());
        }
        // A Deck that passes its own controls on, and other devices.
        assert_eq!(steam_input_hint("Steam Deck", 0x138), None);
        assert_eq!(steam_input_hint("Steam Deck", 0x30), None);
        assert_eq!(steam_input_hint("Pixel 8", 0), None);
        assert_eq!(steam_input_hint("Deck", 0), None);
    }
    #[test]
    fn the_steam_deck_controller_setting_falls_back_to_auto() {
        assert_eq!(deck_pad("auto"), DeckPad::Auto);
        assert_eq!(deck_pad("steam_deck"), DeckPad::SteamDeck);
        assert_eq!(deck_pad("virtual_pad"), DeckPad::VirtualPad);
        assert_eq!(deck_pad("dualsense"), DeckPad::Auto);
        let policy = Policy::resolve(&Default::default()).unwrap();
        assert_eq!(policy.steam_deck, DeckPad::Auto);
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
