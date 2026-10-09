//! Keyboard and mouse injection, batching, and held-key ownership.

use super::*;

impl Injector {
    /// Inject input now.
    fn send(&self, inputs: &[INPUT]) -> Result<()> {
        if (self.inject)(inputs) < inputs.len() {
            bail!(
                "Windows input injection failed: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }
    /// Send the pass's keyboard and mouse input, and record the presses
    /// Windows took. A refused press is not held.
    pub(super) fn flush(&mut self) {
        if self.pending.inputs.is_empty() {
            return;
        }
        let sent = (self.inject)(&self.pending.inputs);
        let refused =
            (sent < self.pending.inputs.len()).then(|| std::io::Error::last_os_error().to_string());
        self.pending.inputs.clear();
        let mut events = std::mem::take(&mut self.pending.events);
        for (end, press) in events.drain(..) {
            if end <= sent {
                if let Some(press) = press {
                    self.hold(press);
                }
                continue;
            }
            if let Some(press) = press
                && press.counted
            {
                let mut held = HELD.lock().unwrap();
                let identity = (press.keyboard, press.id);
                match held.get(&identity).copied().unwrap_or(0) {
                    0 | 1 => held.remove(&identity),
                    count => held.insert(identity, count - 1),
                };
            }
            self.pending.errors.push(anyhow::anyhow!(
                "Windows input injection failed: {}",
                refused.as_deref().unwrap_or_default()
            ));
        }
        self.pending.events = events;
    }
    /// Record a press Windows took.
    pub(super) fn hold(&mut self, press: Press) {
        if !press.keyboard {
            self.buttons.insert(press.id as u8);
            return;
        }
        self.keys.insert(press.id);
        self.key_flags.insert(press.id, press.flags);
        if let Some(modifiers) = press.repeat
            && let Some(delay) = self.policy.repeat_delay
        {
            self.repeat = Some((
                press.id,
                press.flags,
                modifiers,
                std::time::Instant::now() + delay,
            ));
        }
    }
    /// Whether a key (or button) press of the pass is still unsent: what
    /// recording it changes decides how a later key (or button) is sent.
    pub(super) fn press_pending(&self, keyboard: bool) -> bool {
        self.pending
            .events
            .iter()
            .any(|(_, press)| press.as_ref().is_some_and(|p| p.keyboard == keyboard))
    }
    pub(super) fn key(key: u16, down: bool, unicode: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(if unicode { 0 } else { key }),
                    wScan: if unicode { key } else { 0 },
                    dwFlags: (if down {
                        KEYBD_EVENT_FLAGS(0)
                    } else {
                        KEYEVENTF_KEYUP
                    }) | if unicode {
                        KEYEVENTF_UNICODE
                    } else if legacy_extended_key(key) {
                        KEYEVENTF_EXTENDEDKEY
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    ..Default::default()
                },
            },
        }
    }
    pub(super) fn key_scan(&self, key: u32, down: bool, flags: u8) -> INPUT {
        Self::keyboard_input(key, down, flags, self.policy.always_send_scancodes)
    }
    fn keyboard_input(key: u32, down: bool, flags: u8, always_send_scancodes: bool) -> INPUT {
        let extended = key & EXPLICIT_EXTENDED_KEY != 0;
        let key = key as u16;
        let mut input = Self::key(key, down, false);
        if extended {
            // SAFETY: Self::key constructed a keyboard INPUT, so its ki union field is initialized.
            unsafe { input.Anonymous.ki.dwFlags |= KEYEVENTF_EXTENDEDKEY };
        }
        // Normalized Moonlight VKs always use the fixed US table. Non-normalized
        // keys follow the host's layout only when the administrator asks for it.
        let scan = if flags & 1 == 0 {
            crate::keylayout::SCANCODES[(key & 255) as usize] as u16
        } else if always_send_scancodes && !matches!(key, 0x5b | 0x5c | 0x13) {
            // SAFETY: MapVirtualKeyW takes a virtual-key value and a valid mapping constant, with no borrowed pointers.
            unsafe { MapVirtualKeyW(u32::from(key), MAPVK_VK_TO_VSC) as u16 }
        } else {
            0
        };
        if scan != 0 {
            // SAFETY: Self::key constructed a keyboard INPUT, so its ki union field is initialized.
            unsafe {
                input.Anonymous.ki.wVk = VIRTUAL_KEY(0);
                input.Anonymous.ki.wScan = scan;
                input.Anonymous.ki.dwFlags |= KEYEVENTF_SCANCODE;
            }
        }
        input
    }
    pub(super) fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data,
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        }
    }
    pub(super) fn button(button: u8, down: bool) -> INPUT {
        let flag = match (button, down) {
            (1, true) => MOUSEEVENTF_LEFTDOWN,
            (1, false) => MOUSEEVENTF_LEFTUP,
            (2, true) => MOUSEEVENTF_MIDDLEDOWN,
            (2, false) => MOUSEEVENTF_MIDDLEUP,
            (3, true) => MOUSEEVENTF_RIGHTDOWN,
            (3, false) => MOUSEEVENTF_RIGHTUP,
            (_, true) => MOUSEEVENTF_XDOWN,
            (_, false) => MOUSEEVENTF_XUP,
        };
        Self::mouse(
            0,
            0,
            if button >= 4 {
                u32::from(button - 3)
            } else {
                0
            },
            flag,
        )
    }
    /// What is due to the millisecond: the delayed left release after
    /// absolute input and key repeat. The control loop calls this on every
    /// pass; on its 8 ms refresh tick a release landed 10-18 ms after the
    /// client's, not 10.
    pub fn due(&mut self) -> Result<()> {
        let now = std::time::Instant::now();
        let mut first = None;
        if self.left_release.is_some_and(|due| now >= due) {
            self.left_release = None;
            if let Err(error) = self.held(false, 1, false, true, Self::button(1, false)) {
                first.get_or_insert(error);
            }
        }
        if let Some((key, flags, modifiers, due)) = self.repeat
            && now >= due
        {
            self.repeat = Some((key, flags, modifiers, now + self.policy.repeat_period));
            let inputs = Self::repeat_inputs(
                key,
                flags,
                modifiers,
                &self.keys,
                &HELD.lock().unwrap(),
                self.policy.always_send_scancodes,
            );
            if let Err(error) = self.send(&inputs) {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    }
    /// Modifiers in a key-down packet that neither this client nor another
    /// holds as keys (Moonlight reports Shift/Ctrl/Alt both ways).
    pub(super) fn synthetic_modifiers(&self, key: u32, reported: u8) -> u8 {
        unheld_modifiers(key, reported, &self.keys, &HELD.lock().unwrap())
    }
    /// The key's input surrounded by temporary presses of `modifiers`.
    pub(super) fn with_modifiers(
        key: INPUT,
        modifiers: u8,
        always_send_scancodes: bool,
    ) -> Vec<INPUT> {
        let generic = [
            (MODIFIER_SHIFT, 0x10),
            (MODIFIER_CTRL, 0x11),
            (MODIFIER_ALT, 0x12),
        ];
        let mut inputs = Vec::with_capacity(7);
        for (mask, vk) in generic {
            if modifiers & mask != 0 {
                inputs.push(Self::keyboard_input(vk, true, 0, always_send_scancodes));
            }
        }
        inputs.push(key);
        for (mask, vk) in generic.iter().rev() {
            if modifiers & mask != 0 {
                inputs.push(Self::keyboard_input(*vk, false, 0, always_send_scancodes));
            }
        }
        inputs
    }
    /// One repeat of a held key. The modifiers pressed around it at key-down
    /// are pressed again only while still no client holds them: the repeat
    /// must not release a Shift the user pressed since.
    fn repeat_inputs(
        key: u32,
        flags: u8,
        modifiers: u8,
        keys: &BTreeSet<u32>,
        held: &BTreeMap<(bool, u32), usize>,
        always_send_scancodes: bool,
    ) -> Vec<INPUT> {
        Self::with_modifiers(
            Self::keyboard_input(key, true, flags, always_send_scancodes),
            unheld_modifiers(key, modifiers, keys, held),
            always_send_scancodes,
        )
    }
    pub(super) fn held(
        &self,
        keyboard: bool,
        id: u32,
        down: bool,
        owned: bool,
        input: INPUT,
    ) -> Result<()> {
        Self::update_held(
            &mut HELD.lock().unwrap(),
            (keyboard, id),
            down,
            owned,
            input,
            |inputs| self.send(inputs),
        )
    }
    pub(super) fn update_held(
        held: &mut BTreeMap<(bool, u32), usize>,
        identity: (bool, u32),
        down: bool,
        owned: bool,
        input: INPUT,
        send: impl FnOnce(&[INPUT]) -> Result<()>,
    ) -> Result<()> {
        let count = held.get(&identity).copied().unwrap_or(0);
        if down {
            // A repeated press of a key or button this client already holds
            // is not sent again, as in Vibepollo: with the host's own repeat
            // it doubled the repeat rate, and a second button-down could
            // become a double-click.
            if count == 0 && !owned {
                send(&[input])?;
            }
            if !owned {
                held.insert(identity, count + 1);
            }
        } else if owned {
            if count <= 1 {
                // Forget the key first: a release Windows refused must not
                // keep it held for every later press and session.
                held.remove(&identity);
                send(&[input])?;
            } else {
                held.insert(identity, count - 1);
            }
        }
        Ok(())
    }
}
/// Inject input, following the input desktop when Windows refuses it: a UAC
/// prompt or the lock screen runs on the secure desktop. Returns how many
/// inputs Windows took, which it takes in order.
pub(super) fn inject(inputs: &[INPUT]) -> usize {
    // SAFETY: The INPUT slice is initialized, remains live throughout SendInput, and uses the exact INPUT structure size.
    let send = |inputs: &[INPUT]| unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    let mut sent = send(inputs) as usize;
    if sent < inputs.len() && follow_input_desktop() {
        sent += send(&inputs[sent..]) as usize;
    }
    sent
}
// Moonlight 6.2 adds MODIFIER_EXTENDED (0x10) for keys such as keypad
// Enter that share a VK with a non-extended key. Keep this identity through
// held-key ownership, repeats and disconnect cleanup without changing the VK.
const MODIFIER_EXTENDED: u8 = 0x10;
pub(super) const EXPLICIT_EXTENDED_KEY: u32 = 1 << 16;
fn legacy_extended_key(key: u16) -> bool {
    // Navigation, Insert/Delete, right Ctrl/Alt, Windows, Apps and keypad
    // divide, as in Sunshine. Print Screen (0x2c) is not: E0 54 maps to
    // nothing, so it never took a screenshot.
    matches!(
        key,
        0x21..=0x28 | 0x2d | 0x2e | 0xa3 | 0xa5 | 0x5b | 0x5c | 0x5d | 0x6f
    )
}
pub(super) fn mapped_keyboard_identity(
    policy: &butterpollo_core::input_policy::Policy,
    key: u16,
    modifiers: u8,
) -> u32 {
    let mapped = policy.key(key);
    // A remapping chooses the destination key's identity; do not turn it
    // into an extended key merely because the source was.
    keyboard_identity(mapped, if mapped == key { modifiers } else { 0 })
}
fn keyboard_identity(key: u16, modifiers: u8) -> u32 {
    u32::from(key)
        | if modifiers & MODIFIER_EXTENDED != 0 && !legacy_extended_key(key) {
            EXPLICIT_EXTENDED_KEY
        } else {
            0
        }
}
const MODIFIER_SHIFT: u8 = 0x01;
const MODIFIER_CTRL: u8 = 0x02;
const MODIFIER_ALT: u8 = 0x04;
pub(super) fn is_modifier(key: u32) -> bool {
    matches!(key as u16, 0x10..=0x12 | 0xa0..=0xa5 | 0x5b | 0x5c)
}
fn modifier_held(
    keys: &BTreeSet<u32>,
    held: &BTreeMap<(bool, u32), usize>,
    alternatives: [u32; 3],
) -> bool {
    keys.iter()
        .any(|key| alternatives.contains(&(key & 0xffff)))
        || held
            .keys()
            .any(|(keyboard, key)| *keyboard && alternatives.contains(&(key & 0xffff)))
}
/// The Shift, Ctrl and Alt of `modifiers` that no client holds as keys, to
/// press around `key`; none around a modifier.
fn unheld_modifiers(
    key: u32,
    modifiers: u8,
    keys: &BTreeSet<u32>,
    held: &BTreeMap<(bool, u32), usize>,
) -> u8 {
    if is_modifier(key) {
        return 0;
    }
    let mut synthetic = 0;
    for (mask, alternatives) in [
        (MODIFIER_SHIFT, [0x10, 0xa0, 0xa1]),
        (MODIFIER_CTRL, [0x11, 0xa2, 0xa3]),
        (MODIFIER_ALT, [0x12, 0xa4, 0xa5]),
    ] {
        if modifiers & mask != 0 && !modifier_held(keys, held, alternatives) {
            synthetic |= mask;
        }
    }
    synthetic
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keyboard(key: u32, down: bool, flags: u8) -> KEYBDINPUT {
        // No input injection or layout API calls: normalized scan codes come
        // from the static US table; non-normalized keys retain their VK.
        // SAFETY: keyboard_input constructs a keyboard INPUT with its ki union field initialized.
        unsafe {
            Injector::keyboard_input(key, down, flags, false)
                .Anonymous
                .ki
        }
    }
    #[test]
    fn moonlight_keypad_enter_retains_its_identity_for_press_repeat_and_release() {
        let enter = keyboard_identity(0x0d, 0);
        let keypad = keyboard_identity(0x0d, MODIFIER_EXTENDED);
        assert_ne!(enter, keypad);
        assert_eq!(
            keyboard_identity(0x0d, MODIFIER_EXTENDED | MODIFIER_SHIFT),
            keypad
        );
        for flags in [0, 1] {
            for down in [true, true, false] {
                let ordinary = keyboard(enter, down, flags);
                let extended = keyboard(keypad, down, flags);
                assert_eq!(ordinary.wVk, extended.wVk);
                assert_eq!(ordinary.wScan, extended.wScan);
                assert_eq!(
                    ordinary.dwFlags & KEYEVENTF_EXTENDEDKEY,
                    KEYBD_EVENT_FLAGS(0)
                );
                assert_eq!(extended.dwFlags, ordinary.dwFlags | KEYEVENTF_EXTENDEDKEY);
                assert_eq!(
                    extended.dwFlags & KEYEVENTF_KEYUP,
                    if down {
                        KEYBD_EVENT_FLAGS(0)
                    } else {
                        KEYEVENTF_KEYUP
                    }
                );
            }
        }
    }
    #[test]
    fn simultaneous_enter_keys_and_shared_clients_release_independently() {
        let enter = keyboard_identity(0x0d, 0);
        let keypad = keyboard_identity(0x0d, MODIFIER_EXTENDED);
        let mut held = BTreeMap::new();
        let mut sent = Vec::new();
        for (key, down, owned) in [
            (enter, true, false),
            (keypad, true, false),
            (keypad, true, true), // A repeated press is neither sent nor counted.
            (enter, true, false), // A second client also holds ordinary Enter.
            (enter, false, true), // First client must not release that key yet.
            (keypad, false, true),
            (enter, false, true),
        ] {
            Injector::update_held(
                &mut held,
                (true, key),
                down,
                owned,
                Injector::keyboard_input(key, down, 0, false),
                |inputs| {
                    sent.extend(
                        inputs
                            .iter()
                            // SAFETY: Every input here was constructed by keyboard_input, so its ki union field is initialized.
                            .map(|input| unsafe { input.Anonymous.ki.dwFlags }),
                    );
                    Ok(())
                },
            )
            .unwrap();
        }
        assert!(held.is_empty());
        assert_eq!(
            sent,
            vec![
                KEYEVENTF_SCANCODE,
                KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY,
                KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP,
                KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP,
            ]
        );
    }
    #[test]
    fn a_release_windows_refuses_still_forgets_the_key() {
        let key = (true, keyboard_identity(0x0d, 0));
        let mut held = BTreeMap::from([(key, 1)]);
        let refused = Injector::update_held(
            &mut held,
            key,
            false,
            true,
            Injector::keyboard_input(key.1, false, 0, false),
            |_| anyhow::bail!("the secure desktop refused input"),
        );
        assert!(refused.is_err());
        // Otherwise every later press of the key, in every later session, is
        // skipped as already held.
        assert!(held.is_empty());
    }
    #[test]
    fn key_remapping_uses_the_destination_extension_and_legacy_vk_width() {
        let config =
            butterpollo_core::config::Config::parse("keybindings=[13,65,66,163,67,4660]\n")
                .unwrap();
        let policy = butterpollo_core::input_policy::Policy::resolve(&config).unwrap();
        assert_eq!(
            mapped_keyboard_identity(&policy, 0x0d, MODIFIER_EXTENDED),
            0x41
        );
        let right_ctrl = mapped_keyboard_identity(&policy, 0x42, 0);
        assert_eq!(
            keyboard(right_ctrl, true, 0).dwFlags & KEYEVENTF_EXTENDEDKEY,
            KEYEVENTF_EXTENDEDKEY
        );
        assert_eq!(
            mapped_keyboard_identity(&policy, 0x43, MODIFIER_EXTENDED),
            0x1234
        );
        assert_eq!(keyboard_identity(0x1234, MODIFIER_EXTENDED) as u16, 0x1234);
    }
    #[test]
    fn modifier_tracking_ignores_the_internal_extension_bit() {
        let ctrl = keyboard_identity(0x11, MODIFIER_EXTENDED);
        assert!(is_modifier(ctrl));
        let alternatives = [0x11, 0xa2, 0xa3];
        let empty_keys = BTreeSet::new();
        let empty_held = BTreeMap::new();
        assert!(modifier_held(
            &BTreeSet::from([ctrl]),
            &empty_held,
            alternatives
        ));
        assert!(modifier_held(
            &empty_keys,
            &BTreeMap::from([((true, ctrl), 1)]),
            alternatives
        ));
        assert!(!modifier_held(
            &empty_keys,
            &BTreeMap::from([((false, ctrl), 1)]),
            alternatives
        ));
        assert!(!modifier_held(
            &BTreeSet::from([keyboard_identity(0x0d, MODIFIER_EXTENDED)]),
            &empty_held,
            alternatives
        ));
    }
    #[test]
    fn a_key_repeat_does_not_release_a_shift_pressed_after_the_key() {
        let a = keyboard_identity(0x41, 0);
        let mut keys = BTreeSet::from([a]);
        let held = BTreeMap::from([((true, a), 1)]);
        // `a` went down with Moonlight's Shift flag and no Shift key held:
        // Shift is pressed around it.
        let modifiers = unheld_modifiers(a, MODIFIER_SHIFT, &keys, &held);
        assert_eq!(modifiers, MODIFIER_SHIFT);
        let sent = |inputs: Vec<INPUT>| -> Vec<_> {
            inputs
                .iter()
                // SAFETY: repeat_inputs constructs only keyboard INPUTs, so each ki union field is initialized.
                .map(|input| unsafe { (input.Anonymous.ki.wScan, input.Anonymous.ki.dwFlags) })
                .collect()
        };
        let (shift, key) = (
            u16::from(crate::keylayout::SCANCODES[0x10]),
            u16::from(crate::keylayout::SCANCODES[0x41]),
        );
        assert_eq!(
            sent(Injector::repeat_inputs(
                a, 0, modifiers, &keys, &held, false
            )),
            [
                (shift, KEYEVENTF_SCANCODE),
                (key, KEYEVENTF_SCANCODE),
                (shift, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP),
            ]
        );
        // The user then holds the real left Shift: a repeat that released it
        // would let go of the user's Shift while it is still down.
        keys.insert(0xa0);
        let repeat = sent(Injector::repeat_inputs(
            a, 0, modifiers, &keys, &held, false,
        ));
        assert_eq!(repeat, [(key, KEYEVENTF_SCANCODE)]);
        assert!(
            repeat
                .iter()
                .all(|(_, flags)| *flags & KEYEVENTF_KEYUP == KEYBD_EVENT_FLAGS(0))
        );
        // A Shift another client holds counts too.
        let shared = BTreeMap::from([((true, a), 1), ((true, 0xa1), 1)]);
        assert_eq!(
            sent(Injector::repeat_inputs(
                a,
                0,
                modifiers,
                &BTreeSet::from([a]),
                &shared,
                false
            )),
            [(key, KEYEVENTF_SCANCODE)]
        );
        // A modifier that was really held at key-down is not added later.
        assert_eq!(
            sent(Injector::repeat_inputs(
                a,
                0,
                0,
                &BTreeSet::from([a]),
                &held,
                false
            )),
            [(key, KEYEVENTF_SCANCODE)]
        );
    }
    #[test]
    fn new_extended_modifier_preserves_legacy_navigation_and_right_modifier_identity() {
        for key in [
            0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x2d, 0x2e, 0xa3, 0xa5, 0x5b, 0x5c,
            0x5d, 0x6f,
        ] {
            let legacy = keyboard_identity(key, 0);
            assert_eq!(legacy, keyboard_identity(key, MODIFIER_EXTENDED));
            for down in [true, false] {
                assert_eq!(
                    keyboard(legacy, down, 0).dwFlags & KEYEVENTF_EXTENDEDKEY,
                    KEYEVENTF_EXTENDEDKEY
                );
            }
        }
        for key in [0x0d, 0x2c, 0x41, 0xa0, 0xa1, 0xa2, 0xa4] {
            assert_eq!(
                keyboard(keyboard_identity(key, 0), true, 0).dwFlags & KEYEVENTF_EXTENDEDKEY,
                KEYBD_EVENT_FLAGS(0)
            );
        }
    }
}
