use crate::{config::Config, input::Input};
use anyhow::Result;
use std::{collections::BTreeMap, time::Duration};

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
    pub repeat_delay: Duration,
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
            repeat_delay: Duration::from_millis(
                config.integer("key_repeat_delay", 500).clamp(0, 60000) as u64,
            ),
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
        let desired = if configured != 0 {
            configured
        } else if kind == 2 {
            6
        } else if kind == 3 {
            7
        } else if (self.motion_as_ds4 && capabilities & 0x30 != 0)
            || (self.touchpad_as_ds4 && capabilities & 8 != 0)
        {
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
    fn independent_controllers_match_client_type_and_available_driver_profiles() {
        let policy = Policy::resolve(&Config::default()).unwrap();
        let all = (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6);
        assert_eq!(policy.controller_profile(0, 1, 0x4, all), Some(4));
        assert_eq!(policy.controller_profile(0, 2, 0, all), Some(6));
        assert_eq!(policy.controller_profile(0, 3, 0x30, all), Some(7));
        assert_eq!(policy.controller_profile(0, 0, 8, all), Some(6));
        assert_eq!(policy.controller_profile(5, 3, 0, all), Some(5));
        assert_eq!(policy.controller_profile(0, 2, 0, 1 << 3), Some(4));
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
