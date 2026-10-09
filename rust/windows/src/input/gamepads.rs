//! Virtual gamepad lifecycle, controller reports, and feedback.

use super::*;

pub fn open_interface(guid: GUID) -> Result<HANDLE> {
    // SAFETY: The sized SetupAPI structures and aligned detail buffer remain live through each call; the terminated device path is consumed before the device set is destroyed.
    unsafe {
        let set = SetupDiGetClassDevsW(
            Some(&guid),
            PCWSTR::null(),
            None,
            DIGCF_DEVICEINTERFACE | DIGCF_PRESENT,
        )?;
        let result = (|| -> Result<HANDLE> {
            let mut interface = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            SetupDiEnumDeviceInterfaces(set, None, &guid, 0, &mut interface)?;
            let mut size = 0;
            let _ =
                SetupDiGetDeviceInterfaceDetailW(set, &interface, None, 0, Some(&mut size), None);
            if !(8..=65536).contains(&size) {
                bail!("invalid device interface path length");
            }
            let mut data = vec![0u64; (size as usize).div_ceil(8)];
            let detail = data.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            SetupDiGetDeviceInterfaceDetailW(set, &interface, Some(detail), size, None, None)?;
            Ok(CreateFileW(
                PCWSTR((*detail).DevicePath.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )?)
        })();
        SetupDiDestroyDeviceInfoList(set)?;
        result
    }
}
pub(super) fn request(size: u32, id: Option<u32>) -> Vec<u8> {
    let mut b = size.to_le_bytes().to_vec();
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    if let Some(id) = id {
        b.extend_from_slice(&id.to_le_bytes());
    }
    b
}
pub struct Gamepads {
    backend: gamepad_backend::Backend,
    pub(super) active: BTreeMap<u16, u16>,
    profile: u16,
    available: u32,
    profiles: BTreeMap<u16, u16>,
    arrivals: BTreeMap<u16, (u8, u16)>,
    states: BTreeMap<u16, (Event, butterpollo_core::input_policy::BackButton)>,
    policy: butterpollo_core::input_policy::Policy,
    started: std::time::Instant,
    pointers: BTreeMap<(u8, u32), u8>,
    unsupported_touchpads: BTreeSet<u8>,
    /// The last feedback report forwarded for each controller.
    last_feedback: BTreeMap<u16, (u16, Vec<u8>)>,
    feedback_failures: BTreeMap<u16, FeedbackFailure>,
    /// Steam Deck clients' controllers attached as real Steam Decks.
    pub(super) decks: BTreeMap<u16, steam_deck_pad::DeckPad>,
    /// Controllers a Steam Deck could not be attached for; they keep their
    /// VHF pad until they arrive again.
    deck_failed: BTreeSet<u16>,
    warnings: std::sync::Arc<butterpollo_core::session::Warnings>,
}
#[derive(Default)]
struct FeedbackFailure {
    unreported: u64,
    reported: Option<Instant>,
}
static SLOTS: std::sync::Mutex<[bool; 16]> = std::sync::Mutex::new([false; 16]);
impl Gamepads {
    pub fn open(profile: u16) -> Result<Self> {
        Self::open_options(
            profile,
            butterpollo_core::input_policy::Policy::resolve(&Default::default())?,
            Default::default(),
        )
    }
    pub(super) fn open_options(
        profile: u16,
        policy: butterpollo_core::input_policy::Policy,
        warnings: std::sync::Arc<butterpollo_core::session::Warnings>,
    ) -> Result<Self> {
        let (backend, available) = gamepad_backend::Backend::open(profile)?;
        Ok(Self {
            backend,
            active: BTreeMap::new(),
            profile: if profile == VHF_AUTO { 0 } else { profile },
            available,
            profiles: BTreeMap::new(),
            arrivals: BTreeMap::new(),
            states: BTreeMap::new(),
            policy,
            started: std::time::Instant::now(),
            pointers: BTreeMap::new(),
            unsupported_touchpads: BTreeSet::new(),
            last_feedback: BTreeMap::new(),
            feedback_failures: BTreeMap::new(),
            decks: BTreeMap::new(),
            deck_failed: BTreeSet::new(),
            warnings,
        })
    }
    /// Attaches a real Steam Deck controller for a Steam Deck client when
    /// the `steam_deck_controller` setting and the host allow it. Returns
    /// whether controller `id` is one.
    fn ensure_deck(&mut self, id: u16) -> Result<bool> {
        use butterpollo_core::input_policy::{CTYPE_STEAM, DeckPad};
        if self.decks.contains_key(&id) {
            return Ok(true);
        }
        let steam = self
            .arrivals
            .get(&id)
            .is_some_and(|(kind, _)| *kind == CTYPE_STEAM);
        if !steam || self.deck_failed.contains(&id) {
            return Ok(false);
        }
        let mode = self.policy.steam_deck;
        let exe = match mode {
            DeckPad::VirtualPad => None,
            _ => steam_deck_pad::usbip_exe(),
        };
        let Some(exe) = exe else {
            if mode == DeckPad::SteamDeck {
                self.warnings.set("input_steam_deck", "Steam Deck controller unavailable: usbip-win2 is not installed, so the Deck's controls arrive as a virtual pad. Install usbip-win2 on the host to pass the Deck through as a Steam Deck, then reconnect.");
            }
            self.deck_failed.insert(id);
            return Ok(false);
        };
        if mode == DeckPad::Auto && !steam_deck_pad::steam_running() {
            tracing::info!(
                controller = id,
                "Steam is not running; the Steam Deck's controls use a virtual pad"
            );
            self.deck_failed.insert(id);
            return Ok(false);
        }
        let deck = match steam_deck_pad::DeckPad::plug(&exe, &format!("BUTTERPOLLO{id}")) {
            Ok(deck) => deck,
            Err(error) => {
                self.warnings.set("input_steam_deck", format!("Steam Deck controller unavailable ({error:#}); the Deck's controls arrive as a virtual pad. Check that usbip-win2 is installed and working, then reconnect."));
                self.deck_failed.insert(id);
                return Ok(false);
            }
        };
        self.warnings.clear("input_steam_deck");
        tracing::info!(
            controller = id,
            port = deck.port(),
            "Steam Deck controller attached through usbip-win2"
        );
        // A VHF pad plugged before the arrival made way for it.
        if let Some(global) = self.active.remove(&id) {
            self.backend.unplug(u32::from(global))?;
            self.profiles.remove(&id);
            self.states.remove(&id);
            self.last_feedback.remove(&id);
            self.feedback_failures.remove(&id);
            self.pointers.retain(|(pad, _), _| u16::from(*pad) != id);
            SLOTS.lock().unwrap()[global as usize] = false;
        }
        self.decks.insert(id, deck);
        Ok(true)
    }
    fn ensure(&mut self, id: u16) -> Result<()> {
        if id >= 16 {
            bail!("controller ID out of range");
        }
        let (kind, capabilities) = self.arrivals.get(&id).copied().unwrap_or_default();
        let profile = self
            .policy
            .controller_profile(self.profile, kind, capabilities, self.available)
            .ok_or_else(|| anyhow::anyhow!("gamepad driver has no supported controller profile"))?;
        if let Some(global) = self.active.get(&id).copied() {
            if self.profiles.get(&id) == Some(&profile) {
                return Ok(());
            }
            // Some clients send arrival capabilities after their first state.
            self.backend.unplug(u32::from(global))?;
            self.active.remove(&id);
            self.profiles.remove(&id);
            self.last_feedback.remove(&id);
            self.feedback_failures.remove(&id);
            self.pointers.retain(|(pad, _), _| u16::from(*pad) != id);
            SLOTS.lock().unwrap()[global as usize] = false;
        }
        let mut slots = SLOTS.lock().unwrap();
        for global in 0..16u16 {
            if slots[global as usize] {
                continue;
            }
            if self.backend.plug(u32::from(global), profile).is_ok() {
                slots[global as usize] = true;
                self.active.insert(id, global);
                self.profiles.insert(id, profile);
                tracing::info!(
                    controller = id,
                    client_type = kind,
                    capabilities = format!("{capabilities:#x}"),
                    backend = self.backend.name(),
                    profile = profile_name(profile),
                    "virtual controller connected"
                );
                return Ok(());
            }
        }
        bail!("no free virtual controller slots")
    }
    pub fn apply(&mut self, event: &Event) -> Result<()> {
        match event {
            Event::Controller {
                id,
                active,
                buttons,
                left_trigger,
                right_trigger,
                sticks,
            } => {
                if *id >= 16 {
                    bail!("controller ID out of range");
                }
                self.decks.retain(|i, _| active & (1 << *i) != 0);
                for (i, global) in self.active.clone() {
                    if active & (1 << i) == 0 {
                        // A pad that would not unplug stays tracked and is
                        // tried again with the next state; this state still
                        // goes to the live pad.
                        if let Err(error) = self.backend.unplug(u32::from(global)) {
                            tracing::debug!(
                                controller = i,
                                error = format!("{error:#}"),
                                "virtual controller removal failed"
                            );
                            continue;
                        }
                        self.active.remove(&i);
                        self.profiles.remove(&i);
                        self.arrivals.remove(&i);
                        self.states.remove(&i);
                        self.last_feedback.remove(&i);
                        self.feedback_failures.remove(&i);
                        self.pointers.retain(|(id, _), _| u16::from(*id) != i);
                        self.unsupported_touchpads.remove(&(i as u8));
                        SLOTS.lock().unwrap()[global as usize] = false;
                    }
                }
                if active & (1 << id) == 0 {
                    return Ok(());
                }
                if let Some(deck) = self.decks.get(id) {
                    deck.apply(event);
                    return Ok(());
                }
                self.ensure(*id)?;
                // No virtual pad has back grips: press what each is mapped to.
                let (buttons, left_trigger, right_trigger) =
                    self.policy
                        .map_back_grips(*buttons, *left_trigger, *right_trigger);
                let mapped = Event::Controller {
                    id: *id,
                    active: *active,
                    buttons,
                    left_trigger,
                    right_trigger,
                    sticks: *sticks,
                };
                let state = self
                    .states
                    .entry(*id)
                    .or_insert_with(|| (mapped.clone(), Default::default()));
                state.0 = mapped;
                let buttons = state.1.update(
                    buttons,
                    self.started.elapsed(),
                    self.policy.back_button_timeout,
                );
                self.submit(*id, buttons, left_trigger, right_trigger, sticks)?;
            }
            Event::Arrival {
                id,
                kind,
                capabilities,
                ..
            } => {
                let id = u16::from(*id);
                self.arrivals.insert(id, (*kind, *capabilities));
                // A controller that arrives again may try a Steam Deck again.
                self.deck_failed.remove(&id);
                if !self.ensure_deck(id)? {
                    self.ensure(id)?;
                }
            }
            Event::Motion { id, kind, xyz } => {
                if let Some(deck) = self.decks.get(&u16::from(*id)) {
                    deck.apply(event);
                    return Ok(());
                }
                // Only arrival and state packets create a pad: motion travels on
                // another channel and can arrive after the pad was removed.
                if !self.motion_supported(u16::from(*id)) {
                    return Ok(());
                }
                self.backend
                    .motion(self.slot(u16::from(*id))?, *kind, xyz)?;
            }
            Event::ControllerTouch { id, .. } if self.decks.contains_key(&u16::from(*id)) => {
                self.decks[&u16::from(*id)].apply(event);
            }
            Event::ControllerTouch { id, touchpad, .. } => {
                // The driver exposes one PlayStation touch surface with two
                // contacts. A client that announced two touchpads gets them
                // side by side on it; a second touchpad that was not announced
                // is never reinterpreted as the first.
                let dual = self
                    .arrivals
                    .get(&u16::from(*id))
                    .is_some_and(|(_, capabilities)| capabilities & CCAP_DUAL_TOUCHPAD != 0);
                if *touchpad > u8::from(dual) {
                    if self.unsupported_touchpads.insert(*id) {
                        tracing::warn!(
                            controller = id,
                            touchpad,
                            dual,
                            "controller touchpad ignored: the virtual gamepad driver has one touch surface, shared by two touchpads only when the client announced both"
                        );
                    }
                    return Ok(());
                }
                let (Some(profile), Some(global)) = (
                    self.profiles.get(&u16::from(*id)).copied(),
                    self.active.get(&u16::from(*id)).copied(),
                ) else {
                    return Ok(());
                };
                if let Some(b) = gamepad_touch_request(&self.pointers, profile, global, event, dual)
                {
                    self.backend.touch(&b)?;
                    b.submitted(&mut self.pointers);
                }
            }
            Event::Battery { id, state, percent } => {
                // Only the PlayStation and Switch profiles have a battery.
                if !matches!(self.profiles.get(&u16::from(*id)).copied(), Some(5..=7)) {
                    return Ok(());
                }
                self.backend
                    .battery(self.slot(u16::from(*id))?, *state, *percent)?;
            }
            _ => {}
        }
        Ok(())
    }
    /// New feedback (rumble, lights, trigger effects) for each controller.
    /// A controller with nothing pending or a failed poll does not hide the
    /// others' feedback, and a repeated report is not sent again.
    pub fn feedback(&mut self) -> Vec<(u16, u16, Vec<u8>)> {
        let mut reports = poll_feedback(
            &self.active,
            &mut self.last_feedback,
            &mut self.feedback_failures,
            self.backend.name(),
            Instant::now(),
            |slot| self.backend.feedback(slot),
        );
        for (id, deck) in &mut self.decks {
            if let Some((kind, data)) = deck.feedback() {
                reports.push((*id, kind, data));
            }
        }
        reports
    }
    fn submit(
        &mut self,
        id: u16,
        buttons: u32,
        left: u8,
        right: u8,
        sticks: &[i16; 4],
    ) -> Result<()> {
        self.backend
            .submit(self.slot(id)?, buttons, left, right, sticks)
    }
    /// The driver slot of a plugged-in pad. A pad whose re-plug failed has a
    /// state but no slot, and must not take the host down (panic = abort).
    fn slot(&self, id: u16) -> Result<u32> {
        self.active
            .get(&id)
            .map(|global| u32::from(*global))
            .ok_or_else(|| anyhow::anyhow!("controller {id} is not plugged in"))
    }
    pub(super) fn refresh(&mut self) -> Result<()> {
        let mut updates = Vec::new();
        for (id, (event, back)) in &mut self.states {
            if !self.active.contains_key(id) {
                continue;
            }
            if let Some(buttons) =
                back.poll(self.started.elapsed(), self.policy.back_button_timeout)
                && let Event::Controller {
                    left_trigger,
                    right_trigger,
                    sticks,
                    ..
                } = event
            {
                updates.push((*id, buttons, *left_trigger, *right_trigger, *sticks));
            }
        }
        for (id, buttons, left, right, sticks) in updates {
            self.submit(id, buttons, left, right, &sticks)?;
        }
        Ok(())
    }
    pub fn motion_supported(&self, id: u16) -> bool {
        self.decks.contains_key(&id) || matches!(self.profiles.get(&id).copied(), Some(5..=7))
    }
}
fn poll_feedback(
    active: &BTreeMap<u16, u16>,
    last: &mut BTreeMap<u16, (u16, Vec<u8>)>,
    failures: &mut BTreeMap<u16, FeedbackFailure>,
    backend: &str,
    now: Instant,
    mut poll: impl FnMut(u32) -> Result<Option<(u16, Vec<u8>)>>,
) -> Vec<(u16, u16, Vec<u8>)> {
    let mut output = vec![];
    let due = |failure: &FeedbackFailure| {
        failure
            .reported
            .is_none_or(|at| now.duration_since(at) >= Duration::from_secs(5))
    };
    for (&id, &global) in active {
        let report = match poll(u32::from(global)) {
            Ok(report) => {
                // Report the failures the interval held back once the pad
                // recovers, under the same interval, so none go unlogged.
                if let Some(failure) = failures.get_mut(&id)
                    && failure.unreported > 0
                    && due(failure)
                {
                    tracing::warn!(
                        controller = id,
                        slot = global,
                        backend,
                        failures = failure.unreported,
                        "controller feedback polls recovered after failing"
                    );
                    failure.unreported = 0;
                    failure.reported = Some(now);
                }
                let Some(report) = report else { continue };
                report
            }
            Err(error) => {
                let failure = failures.entry(id).or_default();
                failure.unreported += 1;
                if due(failure) {
                    tracing::warn!(controller = id, slot = global, backend, failures = failure.unreported, error = %format!("{error:#}"), "controller feedback poll failed");
                    failure.unreported = 0;
                    failure.reported = Some(now);
                }
                continue;
            }
        };
        if last.get(&id) != Some(&report) {
            last.insert(id, report.clone());
            output.push((id, report.0, report.1));
        }
    }
    last.retain(|id, _| active.contains_key(id));
    // Keep the warning interval across successful polls too: an intermittent
    // failure must not flood the log. Unplugging starts a fresh pad lifetime.
    failures.retain(|id, _| active.contains_key(id));
    output
}
fn profile_name(profile: u16) -> &'static str {
    match profile {
        3 => "vhf_xbox_one",
        4 => "vhf_xbox",
        5 => "vhf_ds4",
        6 => "vhf_ds5",
        7 => "vhf_switch",
        _ => "vhf",
    }
}
// The driver has one touch surface and two contact slots. Keep this mapping
// independent of device IO so unsupported surfaces cannot change slot state.
pub(super) struct GamepadTouchRequest {
    pub(super) packet: Vec<u8>,
    id: u8,
    pointer: u32,
    slot: u8,
    event: u8,
}
impl GamepadTouchRequest {
    fn submitted(self, pointers: &mut BTreeMap<(u8, u32), u8>) {
        if self.event == 5 {
            pointers.retain(|(pad, _), _| *pad != self.id);
        } else if matches!(self.event, 2 | 4) {
            pointers.remove(&(self.id, self.pointer));
        } else {
            pointers.insert((self.id, self.pointer), self.slot);
        }
    }
}
/// Moonlight's `LI_CCAP_DUAL_TOUCHPAD` controller capability.
const CCAP_DUAL_TOUCHPAD: u16 = 0x100;

/// Maps a controller touch onto the driver's single touch surface. With
/// `dual`, touchpad 0 covers the left half of that surface and touchpad 1 the
/// right half, and a finger is told apart by its touchpad as well as its id.
fn gamepad_touch_request(
    pointers: &BTreeMap<(u8, u32), u8>,
    profile: u16,
    global: u16,
    input: &Event,
    dual: bool,
) -> Option<GamepadTouchRequest> {
    let Event::ControllerTouch {
        id,
        event,
        touchpad,
        pointer,
        x,
        y,
        pressure,
    } = input
    else {
        return None;
    };
    if *touchpad > u8::from(dual) || !matches!(profile, 5 | 6) {
        return None;
    }
    let event = match event {
        0..=4 => *event,
        6 => 4,
        7 => 5,
        _ => return None,
    };
    // Finger ids are small; the top bit tells the second touchpad's apart.
    let pointer = (*pointer & 0x7fff_ffff) | (u32::from(*touchpad) << 31);
    let key = (*id, pointer);
    let slot = if event == 5 {
        0
    } else if let Some(slot) = pointers.get(&key) {
        *slot
    } else if matches!(event, 0 | 1) {
        (0..2).find(|slot| {
            !pointers
                .iter()
                .any(|((pad, _), used)| pad == id && used == slot)
        })?
    } else {
        return None;
    };
    let mut b = request(22, Some(u32::from(global)));
    b.extend_from_slice(&[slot, event]);
    let x = if dual {
        x.clamp(0., 1.) / 2. + f32::from(*touchpad) / 2.
    } else {
        *x
    };
    for f in [x, *y, *pressure] {
        b.extend_from_slice(&((f.clamp(0., 1.) * 65535.) as u16).to_le_bytes());
    }
    b.extend_from_slice(&[0, 0]);
    Some(GamepadTouchRequest {
        packet: b,
        id: *id,
        pointer,
        slot,
        event,
    })
}
impl Drop for Gamepads {
    fn drop(&mut self) {
        for (_, global) in self.active.clone() {
            let _ = self.backend.unplug(u32::from(global));
            SLOTS.lock().unwrap()[global as usize] = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn feedback_failures_warn_per_pad_without_hiding_healthy_reports_or_flooding_logs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("feedback.log");
        let file = std::fs::File::create(&path).unwrap();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || file.try_clone().unwrap())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let active = BTreeMap::from([(0, 2), (1, 5)]);
            let mut last = BTreeMap::new();
            let mut failures = BTreeMap::new();
            let now = Instant::now();
            for millis in [0, 1, 4999, 5000] {
                let output = poll_feedback(
                    &active,
                    &mut last,
                    &mut failures,
                    "fake",
                    now + Duration::from_millis(millis),
                    |slot| {
                        if slot == 2 || millis >= 4999 {
                            bail!("injected poll failure");
                        }
                        Ok(Some((1, vec![7, 8])))
                    },
                );
                assert_eq!(
                    output,
                    if millis == 0 {
                        vec![(1, 1, vec![7, 8])]
                    } else {
                        vec![]
                    }
                );
            }
            assert_eq!(
                poll_feedback(
                    &active,
                    &mut last,
                    &mut failures,
                    "fake",
                    now + Duration::from_millis(5001),
                    |_| Ok(Some((1, vec![7, 8])))
                ),
                vec![(0, 1, vec![7, 8])]
            );
            assert!(
                poll_feedback(
                    &active,
                    &mut last,
                    &mut failures,
                    "fake",
                    now + Duration::from_millis(5002),
                    |_| bail!("intermittent poll failure")
                )
                .is_empty()
            );
            let log = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<_> = log.lines().collect();
            assert_eq!(lines.len(), 3, "{log}");
            for line in &lines {
                assert!(line.contains("WARN"));
                assert!(line.contains("controller feedback poll failed"));
                assert!(line.contains("backend=\"fake\""));
                assert!(line.contains("injected poll failure"));
            }
            assert!(lines[0].contains("controller=0 slot=2"));
            assert!(lines[0].contains("failures=1"));
            assert!(lines[1].contains("controller=1 slot=5"));
            assert!(lines[1].contains("failures=1"));
            assert!(lines[2].contains("controller=0 slot=2"));
            assert!(lines[2].contains("failures=3"));

            poll_feedback(
                &BTreeMap::new(),
                &mut last,
                &mut failures,
                "fake",
                now + Duration::from_millis(5003),
                |_| unreachable!(),
            );
            assert!(last.is_empty());
            assert!(failures.is_empty());
            poll_feedback(
                &active,
                &mut last,
                &mut failures,
                "fake",
                now + Duration::from_millis(5004),
                |_| bail!("replugged pad failure"),
            );
            let log = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<_> = log.lines().collect();
            assert_eq!(lines.len(), 5, "{log}");
            assert!(lines[3..].iter().all(|line| line.contains("failures=1")));

            // Failures held back by the interval are reported once the pads
            // recover, and only once.
            for (millis, result) in [(5005, false), (5006, true), (10_004, true), (10_005, true)] {
                poll_feedback(
                    &active,
                    &mut last,
                    &mut failures,
                    "fake",
                    now + Duration::from_millis(millis),
                    |_| {
                        if result {
                            Ok(None)
                        } else {
                            bail!("held back failure")
                        }
                    },
                );
            }
            let log = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<_> = log.lines().collect();
            assert_eq!(lines.len(), 7, "{log}");
            assert!(lines[5].contains("controller=0 slot=2"));
            assert!(lines[6].contains("controller=1 slot=5"));
            assert!(lines[5..].iter().all(|line| {
                line.contains("controller feedback polls recovered after failing")
                    && line.contains("failures=1")
            }));
            assert!(failures.values().all(|failure| failure.unreported == 0));
        });
    }
    fn submit_touch(
        pointers: &mut BTreeMap<(u8, u32), u8>,
        profile: u16,
        global: u16,
        input: &Event,
    ) -> Option<Vec<u8>> {
        submit_touch_on(pointers, profile, global, input, false)
    }
    fn submit_touch_on(
        pointers: &mut BTreeMap<(u8, u32), u8>,
        profile: u16,
        global: u16,
        input: &Event,
        dual: bool,
    ) -> Option<Vec<u8>> {
        let update = gamepad_touch_request(pointers, profile, global, input, dual)?;
        let packet = update.packet.clone();
        update.submitted(pointers);
        Some(packet)
    }
    #[test]
    fn failed_touch_submission_does_not_acquire_or_release_contact_slots() {
        let mut pointers = BTreeMap::new();
        drop(gamepad_touch_request(&pointers, 6, 2, &touch(2, 0, 1, 17), false).unwrap());
        assert!(pointers.is_empty());
        submit_touch(&mut pointers, 6, 2, &touch(2, 0, 1, 17)).unwrap();
        for event in [2, 4, 6, 7] {
            drop(gamepad_touch_request(&pointers, 6, 2, &touch(2, 0, event, 17), false).unwrap());
            assert_eq!(pointers, BTreeMap::from([((2, 17), 0)]));
        }
    }
    #[test]
    fn ds4_keeps_primary_contact_slots_and_ignores_secondary_surfaces() {
        let mut pointers = BTreeMap::new();
        for pointer in [17, 18] {
            submit_touch(&mut pointers, 5, 2, &touch(2, 0, 1, pointer)).unwrap();
        }
        assert!(submit_touch(&mut pointers, 5, 2, &touch(2, 0, 1, 19)).is_none());
        assert!(submit_touch(&mut pointers, 5, 2, &touch(2, 1, 7, 17)).is_none());
        assert_eq!(pointers.len(), 2);
        submit_touch(&mut pointers, 5, 2, &touch(2, 0, 7, 17)).unwrap();
        assert!(pointers.is_empty());
    }
    #[test]
    fn two_announced_touchpads_share_the_surface_side_by_side() {
        let mut pointers = BTreeMap::new();
        // A Steam Deck's trackpads: the same finger id on each, both down.
        let left = submit_touch_on(&mut pointers, 6, 5, &touch(2, 0, 1, 0), true).unwrap();
        let right = submit_touch_on(&mut pointers, 6, 5, &touch(2, 1, 1, 0), true).unwrap();
        assert_eq!(&left[12..14], &[0, 1]);
        assert_eq!(&right[12..14], &[1, 1]);
        // x = 0.25 of each half is 1/8 of the surface and 5/8 of it.
        assert_eq!(&left[14..16], &(8191u16).to_le_bytes());
        assert_eq!(&right[14..16], &(40959u16).to_le_bytes());
        assert_eq!(pointers.len(), 2);
        // Lifting one finger frees only its contact.
        let up = submit_touch_on(&mut pointers, 6, 5, &touch(2, 1, 2, 0), true).unwrap();
        assert_eq!(&up[12..14], &[1, 2]);
        assert_eq!(pointers, BTreeMap::from([((2, 0), 0)]));
        let moved = submit_touch_on(&mut pointers, 6, 5, &touch(2, 0, 3, 0), true).unwrap();
        assert_eq!(&moved[12..14], &[0, 3]);
        // Neither a third finger nor a third touchpad finds a slot or a place.
        submit_touch_on(&mut pointers, 6, 5, &touch(2, 1, 1, 1), true).unwrap();
        assert!(submit_touch_on(&mut pointers, 6, 5, &touch(2, 0, 1, 1), true).is_none());
        assert!(submit_touch_on(&mut pointers, 6, 5, &touch(2, 2, 1, 2), true).is_none());
        // Cancelling clears both touchpads' contacts.
        submit_touch_on(&mut pointers, 6, 5, &touch(2, 0, 7, 0), true).unwrap();
        assert!(pointers.is_empty());
    }
    #[test]
    fn the_right_touchpad_stays_ignored_when_the_client_did_not_announce_two() {
        let mut pointers = BTreeMap::new();
        assert!(submit_touch_on(&mut pointers, 5, 2, &touch(2, 1, 1, 0), false).is_none());
        let full = submit_touch_on(&mut pointers, 5, 2, &touch(2, 0, 1, 0), false).unwrap();
        // A single touchpad keeps the whole surface.
        assert_eq!(&full[14..16], &(16383u16).to_le_bytes());
        assert_eq!(pointers.len(), 1);
        // Pads without a touch surface take no touch even when announced dual.
        for profile in [3, 4, 7] {
            assert!(submit_touch_on(&mut pointers, profile, 2, &touch(2, 1, 1, 5), true).is_none());
        }
    }
    fn touch(id: u8, touchpad: u8, event: u8, pointer: u32) -> Event {
        Event::ControllerTouch {
            id,
            touchpad,
            event,
            pointer,
            x: 0.25,
            y: 0.5,
            pressure: 1.0,
        }
    }
    #[test]
    fn secondary_touchpad_cannot_move_release_or_cancel_primary_contacts() {
        let mut pointers = BTreeMap::new();
        let first = submit_touch(&mut pointers, 6, 5, &touch(2, 0, 1, 17)).unwrap();
        assert_eq!(first.len(), 22);
        assert_eq!(&first[8..12], &5u32.to_le_bytes());
        assert_eq!(&first[12..], &[0, 1, 255, 63, 255, 127, 255, 255, 0, 0]);
        for index in [1, 2, 255] {
            for event in 0..=7 {
                let before = pointers.clone();
                assert!(submit_touch(&mut pointers, 6, 5, &touch(2, index, event, 17)).is_none());
                assert_eq!(pointers, before);
            }
        }
        let moved = submit_touch(&mut pointers, 6, 5, &touch(2, 0, 3, 17)).unwrap();
        assert_eq!(&moved[12..14], &[0, 3]);
        let second = submit_touch(&mut pointers, 6, 5, &touch(2, 0, 1, 18)).unwrap();
        assert_eq!(&second[12..14], &[1, 1]);
        assert!(submit_touch(&mut pointers, 6, 5, &touch(2, 0, 1, 19)).is_none());
        let released = submit_touch(&mut pointers, 6, 5, &touch(2, 0, 2, 17)).unwrap();
        assert_eq!(&released[12..14], &[0, 2]);
        let reused = submit_touch(&mut pointers, 6, 5, &touch(2, 0, 1, 19)).unwrap();
        assert_eq!(&reused[12..14], &[0, 1]);
    }
    #[test]
    fn virtual_touch_mapping_keeps_controller_lifecycles_and_profiles_separate() {
        let mut pointers = BTreeMap::new();
        for id in [2, 3] {
            submit_touch(&mut pointers, 5, u16::from(id), &touch(id, 0, 1, 17)).unwrap();
        }
        let cancel = submit_touch(&mut pointers, 5, 2, &touch(2, 0, 7, 0)).unwrap();
        assert_eq!(&cancel[12..14], &[0, 5]);
        assert_eq!(pointers, BTreeMap::from([((3, 17), 0)]));
        for profile in [1, 2, 3, 4, 7, 8] {
            assert!(submit_touch(&mut pointers, profile, 3, &touch(3, 0, 7, 0)).is_none());
            assert_eq!(pointers, BTreeMap::from([((3, 17), 0)]));
        }
        let cancel_contact = submit_touch(&mut pointers, 6, 3, &touch(3, 0, 6, 17)).unwrap();
        assert_eq!(&cancel_contact[12..14], &[0, 4]);
        assert!(pointers.is_empty());
    }
}
