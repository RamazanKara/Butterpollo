use anyhow::{Context, Result, bail};
use butterpollo_core::input::Input as Event;
use butterpollo_core::input_policy::{
    VHF_AUTO, VIGEM_DS4, VIGEM_X360, gamepad_profile, vhf_stand_in,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    mem::size_of,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::*,
        Storage::FileSystem::*,
        System::StationsAndDesktops::*,
        UI::{
            Controls::*,
            Input::{KeyboardAndMouse::*, Pointer::*},
            WindowsAndMessaging::*,
        },
    },
    core::{GUID, PCWSTR},
};

mod gamepad_backend;
#[path = "input/gamepad_thread.rs"]
mod gamepad_thread;
mod vigem;
pub use gamepad_thread::{GamepadThread, PadReport};

pub fn open_interface(guid: GUID) -> Result<HANDLE> {
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
fn request(size: u32, id: Option<u32>) -> Vec<u8> {
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
    active: BTreeMap<u16, u16>,
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
    /// Set when an explicit ViGEm choice runs on a VHF pad because ViGEmBus
    /// could not be opened.
    notice: Option<String>,
}
#[derive(Default)]
struct FeedbackFailure {
    unreported: u64,
    reported: Option<Instant>,
}
static SLOTS: std::sync::Mutex<[bool; 16]> = std::sync::Mutex::new([false; 16]);
static HELD: std::sync::Mutex<BTreeMap<(bool, u32), usize>> =
    std::sync::Mutex::new(BTreeMap::new());

pub fn capabilities(config: &butterpollo_core::config::Config) -> u32 {
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    let mut flags = 0;
    if config.boolean("mouse", true)
        && config.boolean("native_pen_touch", true)
        && unsafe {
            GetModuleHandleW(windows::core::w!("user32.dll")).is_ok_and(|module| {
                GetProcAddress(module, windows::core::s!("CreateSyntheticPointerDevice")).is_some()
            })
        }
    {
        flags |= 1;
    }
    if config.boolean("controller", true)
        && !matches!(
            config.get("gamepad", "auto"),
            "vhf_xbox" | "vhf_xbox_one" | "x360" | "vhf_switch"
        )
    {
        // Moonlight has one general controller-touch feature bit; there is
        // no separate host bit for dual touchpads. The virtual DualShock 4 and
        // DualSense have one touch surface, so a client's second touchpad
        // (a Steam Deck's right trackpad) is placed on its right half; apply()
        // ignores any other touchpad.
        flags |= 2;
    }
    flags
}
impl Gamepads {
    pub fn open(profile: u16) -> Result<Self> {
        Self::open_options(
            profile,
            butterpollo_core::input_policy::Policy::resolve(&Default::default())?,
        )
    }
    fn open_options(profile: u16, policy: butterpollo_core::input_policy::Policy) -> Result<Self> {
        let (backend, available) = gamepad_backend::Backend::open(profile)?;
        let chosen = profile;
        let profile = if matches!(backend, gamepad_backend::Backend::Vhf(_)) {
            vhf_stand_in(profile)
        } else {
            profile
        };
        let notice = (profile != chosen).then(|| {
            format!(
                "ViGEmBus is not installed or could not be opened, so controllers use {} in place of {}. Install ViGEmBus, or choose Automatic, to change this.",
                profile_label(profile),
                profile_label(chosen)
            )
        });
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
            notice,
        })
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
            match self.backend.plug(u32::from(global), profile) {
                Ok(()) => {
                    slots[global as usize] = true;
                    self.active.insert(id, global);
                    self.profiles.insert(id, profile);
                    tracing::info!(
                        controller = id,
                        client_type = kind,
                        capabilities = format!("{capabilities:#x}"),
                        backend = self.backend.name_for(u32::from(global)),
                        profile = profile_name(profile),
                        "virtual controller connected"
                    );
                    return Ok(());
                }
                Err(error) if matches!(profile, VIGEM_X360 | VIGEM_DS4) => {
                    return Err(error);
                }
                Err(_) => {}
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
                self.ensure(*id)?;
                let state = self
                    .states
                    .entry(*id)
                    .or_insert_with(|| (event.clone(), Default::default()));
                state.0 = event.clone();
                let buttons = state.1.update(
                    *buttons,
                    self.started.elapsed(),
                    self.policy.back_button_timeout,
                );
                self.submit(*id, buttons, *left_trigger, *right_trigger, sticks)?;
            }
            Event::Arrival {
                id,
                kind,
                capabilities,
                ..
            } => {
                self.arrivals.insert(u16::from(*id), (*kind, *capabilities));
                self.ensure(u16::from(*id))?;
            }
            Event::Motion { id, kind, xyz } => {
                // Only arrival and state packets create a pad: motion travels on
                // another channel and can arrive after the pad was removed.
                if !self.motion_supported(u16::from(*id)) {
                    return Ok(());
                }
                self.backend
                    .motion(self.slot(u16::from(*id))?, *kind, xyz)?;
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
                    self.backend.touch(u32::from(global), &b)?;
                    b.submitted(&mut self.pointers);
                }
            }
            Event::Battery { id, state, percent } => {
                // Only the PlayStation and Switch profiles have a battery.
                if !self.motion_supported(u16::from(*id)) {
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
        poll_feedback(
            &self.active,
            &mut self.last_feedback,
            &mut self.feedback_failures,
            self.backend.name(),
            Instant::now(),
            |slot| self.backend.feedback(slot),
        )
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
    fn refresh(&mut self) -> Result<()> {
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
        self.backend.refresh()?;
        Ok(())
    }
    pub fn motion_supported(&self, id: u16) -> bool {
        matches!(self.profiles.get(&id).copied(), Some(5..=7 | VIGEM_DS4))
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
/// The profile as the console's Controller type list names it.
fn profile_label(profile: u16) -> &'static str {
    match profile {
        VIGEM_X360 => "Xbox 360 (ViGEmBus)",
        VIGEM_DS4 => "DualShock 4 (ViGEmBus)",
        3 => "Xbox One (VHF)",
        4 => "Xbox Series (VHF)",
        5 => "DualShock 4 (VHF)",
        6 => "DualSense (VHF)",
        7 => "Switch Pro (VHF)",
        _ => "Automatic",
    }
}
fn profile_name(profile: u16) -> &'static str {
    match profile {
        VIGEM_X360 => "x360",
        VIGEM_DS4 => "ds4",
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
struct GamepadTouchRequest {
    packet: Vec<u8>,
    position: [f32; 2],
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
    if *touchpad > u8::from(dual) || !matches!(profile, 5 | 6 | VIGEM_DS4) {
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
        position: [x, *y],
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
pub struct Injector {
    keys: BTreeSet<u32>,
    buttons: BTreeSet<u8>,
    touches: BTreeMap<u32, POINTER_TOUCH_INFO>,
    touch_device: Option<HSYNTHETICPOINTERDEVICE>,
    pen_device: Option<HSYNTHETICPOINTERDEVICE>,
    pen: POINTER_PEN_INFO,
    /// When touch contacts and the pen were last injected.
    touch_refreshed: std::time::Instant,
    pen_refreshed: std::time::Instant,
    gamepads: GamepadThread,
    /// The display's desktop rectangle, None until the display exists.
    rect: Option<RECT>,
    policy: butterpollo_core::input_policy::Policy,
    key_flags: BTreeMap<u32, u8>,
    /// Repeating key, its flags, the modifiers pressed around it at key-down
    /// and when it repeats.
    repeat: Option<(u32, u8, u8, std::time::Instant)>,
    scroll: [i32; 2],
    haptics: bool,
    /// The display absolute input maps onto, and when its rectangle was read.
    output: String,
    /// The stream's size: clients place absolute input on the whole stream,
    /// including the bars around a display of another shape.
    stream: Option<(u32, u32)>,
    rect_read: std::time::Instant,
    lookup: Option<Lookup>,
    /// When absolute input was last reported ignored for a missing display.
    display_warned: Option<std::time::Instant>,
    /// Whether the client last moved the mouse by absolute position, and a
    /// left-button release held back meanwhile.
    absolute: bool,
    left_release: Option<std::time::Instant>,
    pending: Pending,
    /// SendInput, or a stand-in in tests.
    inject: fn(&[INPUT]) -> usize,
}
/// Keyboard and mouse input of one pass of the control loop, sent in one
/// SendInput call: a call cost 31-36 us whether it carried one input or
/// eight.
#[derive(Default)]
struct Pending {
    inputs: Vec<INPUT>,
    /// Each event's end in `inputs`, and its press if it is one.
    events: Vec<(usize, Option<Press>)>,
    /// Why events of the pass failed.
    errors: Vec<anyhow::Error>,
}
/// A display looked up on its own thread.
struct Lookup {
    output: String,
    started: std::time::Instant,
    /// None once its result was used.
    thread: Option<std::thread::JoinHandle<Result<RECT>>>,
}
/// A key or button press, recorded as held once Windows takes it.
struct Press {
    keyboard: bool,
    id: u32,
    flags: u8,
    /// Whether it was counted in HELD, to count it out if Windows refuses it.
    counted: bool,
    /// The modifiers pressed around a key that repeats.
    repeat: Option<u8>,
}
/// How long a left release waits after absolute input, as in Sunshine.
const LEFT_RELEASE_DELAY: std::time::Duration = std::time::Duration::from_millis(10);
/// How often a display's rectangle is read again, and a missing display
/// looked up again: the lookup enumerates every display.
const RECT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
/// How often absolute input ignored for a missing display is reported.
const MISSING_DISPLAY_WARNING: std::time::Duration = std::time::Duration::from_secs(10);
/// How often held touch contacts and an active pen are injected again:
/// Windows cancels pointer input that is not repeated.
const POINTER_REFRESH: std::time::Duration = std::time::Duration::from_millis(250);
/// Whether a pointer last injected at `last` is due again at `now`; if so,
/// its period restarts. Touch and pen keep their own periods: a pen that
/// keeps hovering must not hold back the repeat of a held touch, which
/// Windows would then cancel, nor the other way round.
fn refresh_due(last: &mut std::time::Instant, now: std::time::Instant) -> bool {
    let due = now.saturating_duration_since(*last) >= POINTER_REFRESH;
    if due {
        *last = now;
    }
    due
}
/// The desktop rectangle of `output`: a GDI name, a device ID, or empty for
/// the first display.
fn display_rect(output: &str) -> Result<RECT> {
    // Before capture publishes its GDI name, the configured display is
    // often a device ID ({...}); input arriving then was dropped.
    let name = crate::display::monitors()
        .ok()
        .and_then(|all| all.into_iter().find(|m| m.matches(output)))
        .map_or_else(|| output.to_owned(), |m| m.display_name);
    let d = crate::capture::displays()?
        .into_iter()
        .find(|d| output.is_empty() || d.display_name.eq_ignore_ascii_case(&name))
        .ok_or_else(|| anyhow::anyhow!("input display missing"))?;
    Ok(RECT {
        left: d.x,
        top: d.y,
        right: d.x + d.width as i32,
        bottom: d.y + d.height as i32,
    })
}
/// The current rectangle of a display by GDI name, read cheaply from its mode.
fn current_rect(name: &str) -> Option<RECT> {
    if !name.starts_with(r"\\.\") {
        return None;
    }
    let mode = crate::display::mode(name).ok()?;
    let position = unsafe { mode.Anonymous1.Anonymous2.dmPosition };
    Some(RECT {
        left: position.x,
        top: position.y,
        right: position.x + mode.dmPelsWidth as i32,
        bottom: position.y + mode.dmPelsHeight as i32,
    })
}
/// A width and height, and a rectangle (left, top, width, height) in it.
type Size = (u32, u32);
type Picture = (u32, u32, u32, u32);
impl Injector {
    pub fn new(output: &str, profile: &str) -> Result<Self> {
        Self::new_options(
            output,
            profile,
            &butterpollo_core::config::Config::default(),
        )
    }
    pub fn new_options(
        output: &str,
        profile: &str,
        config: &butterpollo_core::config::Config,
    ) -> Result<Self> {
        Self::new_options_reported(output, profile, config, Default::default())
    }
    pub fn new_options_reported(
        output: &str,
        profile: &str,
        config: &butterpollo_core::config::Config,
        warnings: std::sync::Arc<butterpollo_core::session::Warnings>,
    ) -> Result<Self> {
        let profile = gamepad_profile(profile);
        let policy = butterpollo_core::input_policy::Policy::resolve(config)?;
        // Keyboard, relative mouse and controllers must work before the
        // stream's display exists: a controller's arrival is sent only once.
        let mut injector = Self {
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            touches: BTreeMap::new(),
            touch_device: None,
            pen_device: None,
            pen: Default::default(),
            touch_refreshed: std::time::Instant::now(),
            pen_refreshed: std::time::Instant::now(),
            gamepads: GamepadThread::new(profile, policy.clone(), warnings)?,
            policy,
            key_flags: BTreeMap::new(),
            repeat: None,
            scroll: [0; 2],
            haptics: true,
            rect: None,
            output: output.to_owned(),
            stream: None,
            rect_read: std::time::Instant::now(),
            lookup: None,
            display_warned: None,
            absolute: false,
            left_release: None,
            pending: Pending::default(),
            inject,
        };
        injector.look_up(output);
        Ok(injector)
    }
    /// The stream's size, so absolute input skips the bars around a display of
    /// another shape, as the encoder adds them.
    pub fn set_stream_size(&mut self, width: u32, height: u32) {
        self.stream = (width > 0 && height > 0).then_some((width, height));
    }
    /// Where the display's picture sits in the stream, when it does not fill it.
    fn picture(&self) -> Option<(Size, Picture)> {
        let stream = self.stream?;
        let rect = self.rect?;
        let display = (
            u32::try_from(rect.right - rect.left).ok()?,
            u32::try_from(rect.bottom - rect.top).ok()?,
        );
        butterpollo_core::display_policy::picture(display, stream).map(|picture| (stream, picture))
    }
    /// A point on the stream as fractions of the display.
    fn on_display(&self, x: f32, y: f32) -> (f32, f32) {
        match self.picture() {
            Some((stream, picture)) => {
                let (x, y) = butterpollo_core::display_policy::stream_to_picture(
                    (f64::from(x), f64::from(y)),
                    stream,
                    picture,
                );
                (x as f32, y as f32)
            }
            None => (x, y),
        }
    }
    /// Map absolute input onto `output` from now on: the stream's display can
    /// be created, recreated or renamed after input began.
    pub fn set_output(&mut self, output: &str) {
        if output.is_empty() {
            return;
        }
        if self
            .lookup
            .as_ref()
            .is_some_and(|lookup| !lookup.output.eq_ignore_ascii_case(output))
        {
            self.lookup = None;
        }
        self.settle_lookup(false);
        let renamed = !output.eq_ignore_ascii_case(&self.output);
        if self.rect.is_none() {
            // Until the display exists, follow the session's name, and look
            // the same name up again only every RECT_INTERVAL.
            if renamed || self.rect_read.elapsed() >= RECT_INTERVAL {
                self.output = output.to_owned();
                self.resolve_rect();
            }
            return;
        }
        // The rectangle and the name change together once it is found; a
        // name not found is looked up again every RECT_INTERVAL.
        if renamed
            && self.lookup.as_ref().is_none_or(|lookup| {
                !lookup.output.eq_ignore_ascii_case(output)
                    || lookup.started.elapsed() >= RECT_INTERVAL
            })
        {
            self.look_up(output);
        }
    }
    /// Look up a display that did not exist yet.
    fn resolve_rect(&mut self) {
        self.rect_read = std::time::Instant::now();
        let output = self.output.clone();
        self.look_up(&output);
    }
    /// Look `output` up on another thread, unless it is being looked up: the
    /// lookup enumerates every display, which took 1.5-2.5 ms and once 18 ms
    /// on the input thread.
    fn look_up(&mut self, output: &str) {
        if self.lookup.as_ref().is_some_and(|lookup| {
            lookup.thread.is_some() && lookup.output.eq_ignore_ascii_case(output)
        }) {
            return;
        }
        let name = output.to_owned();
        let thread = std::thread::Builder::new()
            .name("display lookup".into())
            .spawn(move || display_rect(&name));
        self.lookup = Some(Lookup {
            output: output.to_owned(),
            started: std::time::Instant::now(),
            thread: None,
        });
        match thread {
            Ok(thread) => self.lookup.as_mut().unwrap().thread = Some(thread),
            Err(_) => self.found(output.to_owned(), display_rect(output)),
        }
    }
    /// Use the result of a lookup that finished; with `wait`, of one still
    /// running.
    fn settle_lookup(&mut self, wait: bool) {
        let Some(lookup) = &mut self.lookup else {
            return;
        };
        let Some(thread) = lookup.thread.take_if(|thread| wait || thread.is_finished()) else {
            return;
        };
        let output = lookup.output.clone();
        if let Ok(rect) = thread.join() {
            self.found(output, rect);
        }
    }
    fn found(&mut self, output: String, rect: Result<RECT>) {
        match rect {
            Ok(rect) => {
                tracing::debug!(output, "input display found");
                self.rect = Some(rect);
                self.output = output;
                self.rect_read = std::time::Instant::now();
            }
            Err(error) => tracing::debug!(%error, output, "input display unavailable"),
        }
    }
    /// The display's rectangle. Until the display exists, absolute mouse,
    /// touch and pen input is ignored, and that is reported now and then.
    fn display(&mut self) -> Option<RECT> {
        // Absolute input maps onto the display a running lookup is finding,
        // as when the lookup ran here; other input never waits for it.
        self.settle_lookup(true);
        if self.rect.is_none()
            && self
                .display_warned
                .is_none_or(|at| at.elapsed() >= MISSING_DISPLAY_WARNING)
        {
            self.display_warned = Some(std::time::Instant::now());
            tracing::warn!(
                output = %self.output,
                "input display not found yet; absolute mouse, touch and pen input is ignored"
            );
        }
        self.rect
    }
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
    fn flush(&mut self) {
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
    fn hold(&mut self, press: Press) {
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
    fn press_pending(&self, keyboard: bool) -> bool {
        self.pending
            .events
            .iter()
            .any(|(_, press)| press.as_ref().is_some_and(|p| p.keyboard == keyboard))
    }
    fn key(key: u16, down: bool, unicode: bool) -> INPUT {
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
    fn key_scan(&self, key: u32, down: bool, flags: u8) -> INPUT {
        Self::keyboard_input(key, down, flags, self.policy.always_send_scancodes)
    }
    fn keyboard_input(key: u32, down: bool, flags: u8, always_send_scancodes: bool) -> INPUT {
        let extended = key & EXPLICIT_EXTENDED_KEY != 0;
        let key = key as u16;
        let mut input = Self::key(key, down, false);
        if extended {
            unsafe { input.Anonymous.ki.dwFlags |= KEYEVENTF_EXTENDEDKEY };
        }
        // Normalized Moonlight VKs always use the fixed US table. Non-normalized
        // keys follow the host's layout only when the administrator asks for it.
        let scan = if flags & 1 == 0 {
            crate::keylayout::SCANCODES[(key & 255) as usize] as u16
        } else if always_send_scancodes && !matches!(key, 0x5b | 0x5c | 0x13) {
            unsafe { MapVirtualKeyW(u32::from(key), MAPVK_VK_TO_VSC) as u16 }
        } else {
            0
        };
        if scan != 0 {
            unsafe {
                input.Anonymous.ki.wVk = VIRTUAL_KEY(0);
                input.Anonymous.ki.wScan = scan;
                input.Anonymous.ki.dwFlags |= KEYEVENTF_SCANCODE;
            }
        }
        input
    }
    fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
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
    fn button(button: u8, down: bool) -> INPUT {
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
    fn location(&self, rect: RECT, x: f32, y: f32) -> POINT {
        let (x, y) = self.on_display(x, y);
        POINT {
            x: rect.left + ((rect.right - rect.left - 1).max(0) as f32 * x.clamp(0., 1.)) as i32,
            y: rect.top + ((rect.bottom - rect.top - 1).max(0) as f32 * y.clamp(0., 1.)) as i32,
        }
    }
    fn inject_touches(&self) -> Result<()> {
        if let Some(device) = self.touch_device {
            let contacts: Vec<_> = self
                .touches
                .values()
                .map(|touch| POINTER_TYPE_INFO {
                    r#type: PT_TOUCH,
                    Anonymous: POINTER_TYPE_INFO_0 { touchInfo: *touch },
                })
                .collect();
            if !contacts.is_empty() {
                unsafe {
                    inject_pointer(device, &contacts)?;
                }
            }
        }
        Ok(())
    }
    /// Cancel every touch contact, as a client's cancel-all does.
    fn cancel_touches(&mut self) -> Result<()> {
        for p in self.touches.values_mut() {
            pointer_event(&mut p.pointerInfo, 4, POINT::default());
        }
        let result = self.inject_touches();
        self.touches.clear();
        result
    }
    /// Lift the pen if it is active.
    fn cancel_pen(&mut self) -> Result<()> {
        let (Some(device), Some(frame)) = (self.pen_device, pen_cancel(self.pen)) else {
            return Ok(());
        };
        let result = unsafe {
            inject_pointer(
                device,
                &[POINTER_TYPE_INFO {
                    r#type: PT_PEN,
                    Anonymous: POINTER_TYPE_INFO_0 { penInfo: frame },
                }],
            )
        };
        self.pen = pen_after(frame, 7, result.is_err());
        result
    }
    #[allow(clippy::too_many_arguments)]
    fn touch(
        &mut self,
        event: u8,
        id: u32,
        x: f32,
        y: f32,
        pressure: f32,
        major: f32,
        minor: f32,
        rotation: u16,
    ) -> Result<()> {
        // No contact exists before the display does: the rectangle is never
        // forgotten once known.
        let Some(rect) = self.display() else {
            return Ok(());
        };
        unsafe {
            if self.touch_device.is_none() {
                self.touch_device = Some(
                    CreateSyntheticPointerDevice(PT_TOUCH, 32, POINTER_FEEDBACK_NONE)
                        .context("native touch input unavailable")?,
                );
            }
            if event == 7 {
                return self.cancel_touches();
            }
            let mut p = if let Some(existing) = self.touches.get(&id) {
                *existing
            } else {
                if !matches!(event, 0 | 1 | 3) {
                    return Ok(());
                }
                let pointer_id = (1..=32)
                    .find(|candidate| {
                        !self
                            .touches
                            .values()
                            .any(|p| p.pointerInfo.pointerId == *candidate)
                    })
                    .ok_or_else(|| anyhow::anyhow!("touch pointer limit reached"))?;
                POINTER_TOUCH_INFO {
                    pointerInfo: POINTER_INFO {
                        pointerType: PT_TOUCH,
                        pointerId: pointer_id,
                        ..Default::default()
                    },
                    ..Default::default()
                }
            };
            pointer_event(&mut p.pointerInfo, event, self.location(rect, x, y));
            if event != 5 {
                p.touchMask = TOUCH_MASK_NONE;
                if p.pointerInfo.pointerFlags & POINTER_FLAG_INCONTACT != POINTER_FLAG_NONE {
                    p.touchMask |= TOUCH_MASK_PRESSURE;
                    p.pressure = if pressure > 0. {
                        (pressure.clamp(0., 1.) * 1024.) as u32
                    } else {
                        512
                    };
                    if major > 0. || minor > 0. {
                        let angle = if rotation == u16::MAX {
                            45.0f32.to_radians()
                        } else {
                            f32::from(rotation).to_radians()
                        };
                        // The contact's size is given on the stream too.
                        let (scale_x, scale_y) = self.picture().map_or((1., 1.), |(stream, p)| {
                            (
                                stream.0 as f32 / p.2.max(1) as f32,
                                stream.1 as f32 / p.3.max(1) as f32,
                            )
                        });
                        let width = ((angle.cos().abs() * major + angle.sin().abs() * minor)
                            * scale_x
                            * (rect.right - rect.left) as f32)
                            .max(1.);
                        let height = ((angle.sin().abs() * major + angle.cos().abs() * minor)
                            * scale_y
                            * (rect.bottom - rect.top) as f32)
                            .max(1.);
                        let center = p.pointerInfo.ptPixelLocation;
                        p.rcContact = RECT {
                            left: (center.x - (width / 2.).ceil() as i32).max(rect.left),
                            right: (center.x + (width / 2.).ceil() as i32).min(rect.right),
                            top: (center.y - (height / 2.).ceil() as i32).max(rect.top),
                            bottom: (center.y + (height / 2.).ceil() as i32).min(rect.bottom),
                        };
                        p.touchMask |= TOUCH_MASK_CONTACTAREA;
                    }
                }
                if rotation != u16::MAX {
                    p.orientation = u32::from(rotation % 360);
                    p.touchMask |= TOUCH_MASK_ORIENTATION;
                }
            }
            self.touches.insert(id, p);
            let result = self.inject_touches();
            if matches!(event, 2 | 4 | 6) {
                self.touches.remove(&id);
            }
            // Ended contacts are gone, and a contact Windows refused to put
            // down was never seen: drop both whether or not the frame went
            // through, or every later frame repeats the failure.
            let failed = result.is_err();
            self.touches.retain(|_, p| {
                let flags = p.pointerInfo.pointerFlags;
                flags & (POINTER_FLAG_UP | POINTER_FLAG_CANCELED) == POINTER_FLAG_NONE
                    && !(failed && flags & POINTER_FLAG_DOWN != POINTER_FLAG_NONE)
            });
            for p in self.touches.values_mut() {
                p.pointerInfo.pointerFlags &=
                    !(POINTER_FLAG_DOWN | POINTER_FLAG_UP | POINTER_FLAG_CANCELED);
                p.pointerInfo.pointerFlags |= POINTER_FLAG_UPDATE;
            }
            self.touch_refreshed = std::time::Instant::now();
            result?;
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn pen(
        &mut self,
        event: u8,
        tool: u8,
        buttons: u8,
        x: f32,
        y: f32,
        pressure: f32,
        rotation: u16,
        tilt: u8,
    ) -> Result<()> {
        // The pen cannot be active before the display exists.
        let Some(rect) = self.display() else {
            return Ok(());
        };
        let location = self.location(rect, x, y);
        let Some(frame) = pen_frame(
            self.pen, event, tool, buttons, location, pressure, rotation, tilt,
        ) else {
            return Ok(());
        };
        let device = match self.pen_device {
            Some(device) => device,
            None => *self.pen_device.insert(unsafe {
                CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_NONE)
                    .context("native pen input unavailable")?
            }),
        };
        let result = unsafe {
            inject_pointer(
                device,
                &[POINTER_TYPE_INFO {
                    r#type: PT_PEN,
                    Anonymous: POINTER_TYPE_INFO_0 { penInfo: frame },
                }],
            )
        };
        self.pen = pen_after(frame, event, result.is_err());
        self.pen_refreshed = std::time::Instant::now();
        result
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
    pub fn refresh(&mut self) -> Result<()> {
        // Every step runs even when an earlier one fails; the first error is
        // reported. A failed step is not retried before its next due time.
        let mut first = None;
        let mut keep = |result: Result<()>| {
            if let Err(error) = result {
                first.get_or_insert(error);
            }
        };
        keep(self.due());
        let now = std::time::Instant::now();
        // A game can change its display's resolution or position mid-stream,
        // and the stream's display can appear after input began.
        self.settle_lookup(false);
        if self.rect_read.elapsed() >= RECT_INTERVAL {
            if self.rect.is_none() {
                self.resolve_rect();
            } else {
                self.rect_read = now;
                if let Some(rect) = current_rect(&self.output) {
                    self.rect = Some(rect);
                }
            }
        }
        if refresh_due(&mut self.touch_refreshed, now) {
            keep(self.inject_touches());
        }
        if refresh_due(&mut self.pen_refreshed, now)
            && self.pen.pointerInfo.pointerFlags != POINTER_FLAG_NONE
            && let Some(device) = self.pen_device
        {
            keep(unsafe {
                inject_pointer(
                    device,
                    &[POINTER_TYPE_INFO {
                        r#type: PT_PEN,
                        Anonymous: POINTER_TYPE_INFO_0 { penInfo: self.pen },
                    }],
                )
            });
        }
        first.map_or(Ok(()), Err)
    }
    pub fn apply(&mut self, e: &Event) -> Result<()> {
        let mut errors = self.apply_all(std::slice::from_ref(e));
        errors.pop().map_or(Ok(()), Err)
    }
    /// Apply a pass of events in order, with one SendInput call where the
    /// order allows; returns why events failed.
    pub fn apply_all(&mut self, events: &[Event]) -> Vec<anyhow::Error> {
        for e in events {
            if let Err(error) = self.stage(e) {
                self.pending.errors.push(error);
            }
        }
        self.flush();
        std::mem::take(&mut self.pending.errors)
    }
    /// Add an event's keyboard and mouse input to the pass, or apply it.
    fn stage(&mut self, e: &Event) -> Result<()> {
        if !self.policy.allows(e) {
            return Ok(());
        }
        use Event::*;
        match e {
            // How a key or button is sent depends on the keys or buttons held,
            // including those pressed earlier in the pass.
            Keyboard { .. } if self.press_pending(true) => self.flush(),
            MouseButton { .. } if self.press_pending(false) => self.flush(),
            // Touch and pen go through another call: keep them in order.
            Touch { .. } | Pen { .. } => self.flush(),
            _ => {}
        }
        let start = self.pending.inputs.len();
        let press = self.stage_event(e)?;
        let end = self.pending.inputs.len();
        if end > start {
            self.pending.events.push((end, press));
        } else if let Some(press) = press {
            // Nothing to send, such as a key another client holds.
            self.hold(press);
        }
        Ok(())
    }
    fn stage_event(&mut self, e: &Event) -> Result<Option<Press>> {
        use Event::*;
        match e {
            Relative { x, y } => {
                self.absolute = false;
                self.pending.inputs.push(Self::mouse(
                    i32::from(*x),
                    i32::from(*y),
                    0,
                    MOUSEEVENTF_MOVE,
                ));
            }
            Absolute {
                x,
                y,
                width,
                height,
            } => unsafe {
                self.absolute = true;
                let Some(rect) = self.display() else {
                    return Ok(None);
                };
                let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
                let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
                let w = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
                let h = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
                // The client's far edge is the display's last pixel, not the
                // first pixel of its neighbour.
                let (width, height) = (i64::from(*width).max(1), i64::from(*height).max(1));
                let (span_x, span_y) = (
                    i64::from((rect.right - rect.left - 1).max(0)),
                    i64::from((rect.bottom - rect.top - 1).max(0)),
                );
                let (px, py) = match self.picture() {
                    // The client's size is the stream's picture with its bars.
                    Some((stream, picture)) => {
                        let (fx, fy) = butterpollo_core::display_policy::stream_to_picture(
                            (
                                i64::from(*x).clamp(0, width) as f64 / width as f64,
                                i64::from(*y).clamp(0, height) as f64 / height as f64,
                            ),
                            stream,
                            picture,
                        );
                        (
                            rect.left + (fx * span_x as f64) as i32,
                            rect.top + (fy * span_y as f64) as i32,
                        )
                    }
                    None => (
                        rect.left + (i64::from(*x).clamp(0, width) * span_x / width) as i32,
                        rect.top + (i64::from(*y).clamp(0, height) * span_y / height) as i32,
                    ),
                };
                self.pending.inputs.push(Self::mouse(
                    ((i64::from(px - left) * 65535) / i64::from((w - 1).max(1))) as i32,
                    ((i64::from(py - top) * 65535) / i64::from((h - 1).max(1))) as i32,
                    0,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                ));
            },
            MouseButton { button, down } => {
                // With absolute input (touch, pen, a tablet), Moonlight sends a
                // right press right after a left release for a long press: the
                // left click would land first and open a link. Hold the left
                // release back briefly and click right meanwhile, as Sunshine does.
                if *button == 1 && self.absolute {
                    if !*down && self.buttons.contains(button) {
                        self.buttons.remove(button);
                        self.left_release = Some(std::time::Instant::now() + LEFT_RELEASE_DELAY);
                        return Ok(None);
                    }
                    if *down && self.left_release.take().is_some() {
                        // Still down in Windows: its release never went out.
                        self.buttons.insert(*button);
                        return Ok(None);
                    }
                }
                if *button == 3 && *down && self.left_release.is_some() {
                    self.pending
                        .inputs
                        .extend([Self::button(3, true), Self::button(3, false)]);
                    return Ok(None);
                }
                let owned = self.buttons.contains(button);
                let inputs = &mut self.pending.inputs;
                Self::update_held(
                    &mut HELD.lock().unwrap(),
                    (false, u32::from(*button)),
                    *down,
                    owned,
                    Self::button(*button, *down),
                    |input| {
                        inputs.extend_from_slice(input);
                        Ok(())
                    },
                )?;
                if *down {
                    return Ok(Some(Press {
                        keyboard: false,
                        id: u32::from(*button),
                        flags: 0,
                        counted: !owned,
                        repeat: None,
                    }));
                }
                // A release is recorded even when Windows refuses it.
                self.buttons.remove(button);
            }
            Scroll { amount, horizontal } => {
                let index = usize::from(*horizontal);
                let amount = if self.policy.high_resolution_scrolling {
                    i32::from(*amount)
                } else {
                    self.scroll[index] += i32::from(*amount);
                    let amount = self.scroll[index] / 120 * 120;
                    self.scroll[index] -= amount;
                    amount
                };
                if amount == 0 {
                    return Ok(None);
                }
                self.pending.inputs.push(Self::mouse(
                    0,
                    0,
                    amount as u32,
                    if *horizontal {
                        MOUSEEVENTF_HWHEEL
                    } else {
                        MOUSEEVENTF_WHEEL
                    },
                ));
            }
            Keyboard {
                key,
                down,
                flags,
                modifiers,
            } => {
                let mut key = mapped_keyboard_identity(&self.policy, *key, *modifiers);
                // A client can set the extended-key bit on the press but not
                // on the release: release the key that is held, or it stays
                // down and keeps repeating.
                if !*down
                    && !self.keys.contains(&key)
                    && self.keys.contains(&(key ^ EXPLICIT_EXTENDED_KEY))
                {
                    key ^= EXPLICIT_EXTENDED_KEY;
                }
                let owned = self.keys.contains(&key);
                let flags = if *down {
                    *flags
                } else {
                    self.key_flags.get(&key).copied().unwrap_or(*flags)
                };
                let modifiers = if *down && !owned {
                    self.synthetic_modifiers(key, *modifiers)
                } else {
                    0
                };
                let input = self.key_scan(key, *down, flags);
                let always_send_scancodes = self.policy.always_send_scancodes;
                let inputs = &mut self.pending.inputs;
                if modifiers == 0 {
                    Self::update_held(
                        &mut HELD.lock().unwrap(),
                        (true, key),
                        *down,
                        owned,
                        input,
                        |input| {
                            inputs.extend_from_slice(input);
                            Ok(())
                        },
                    )?;
                } else {
                    // The client reported a modifier it never pressed as a
                    // key: press it around this key only, as Vibepollo does.
                    let mut held = HELD.lock().unwrap();
                    let count = held.get(&(true, key)).copied().unwrap_or(0);
                    if count == 0 {
                        inputs.extend(Self::with_modifiers(
                            input,
                            modifiers,
                            always_send_scancodes,
                        ));
                    }
                    held.insert((true, key), count + 1);
                }
                // A press Windows refuses is not held; a release is recorded
                // even when Windows refuses it, or the key keeps repeating.
                if *down {
                    return Ok(Some(Press {
                        keyboard: true,
                        id: key,
                        flags,
                        counted: !owned,
                        repeat: (!owned && !is_modifier(key)).then_some(modifiers),
                    }));
                }
                self.keys.remove(&key);
                self.key_flags.remove(&key);
                if self.repeat.is_some_and(|(repeating, ..)| repeating == key) {
                    self.repeat = None;
                }
            }
            Text(s) => {
                for v in s.encode_utf16() {
                    self.pending
                        .inputs
                        .extend([Self::key(v, true, true), Self::key(v, false, true)]);
                }
            }
            Touch {
                event,
                id,
                x,
                y,
                pressure,
                major,
                minor,
                rotation,
            } => self.touch(*event, *id, *x, *y, *pressure, *major, *minor, *rotation)?,
            Pen {
                event,
                tool,
                buttons,
                x,
                y,
                pressure,
                rotation,
                tilt,
            } => self.pen(*event, *tool, *buttons, *x, *y, *pressure, *rotation, *tilt)?,
            Haptics(enabled) => self.haptics = *enabled,
            Controller { .. }
            | Arrival { .. }
            | ControllerTouch { .. }
            | Motion { .. }
            | Battery { .. } => self.gamepads.send(e.clone()),
        }
        Ok(None)
    }
    /// Feedback and motion-sensor requests from the virtual gamepads for the
    /// client, collected since the last call.
    pub fn gamepad_reports(&self) -> impl Iterator<Item = PadReport> + '_ {
        self.gamepads.reports()
    }
    /// Modifiers in a key-down packet that neither this client nor another
    /// holds as keys (Moonlight reports Shift/Ctrl/Alt both ways).
    fn synthetic_modifiers(&self, key: u32, reported: u8) -> u8 {
        unheld_modifiers(key, reported, &self.keys, &HELD.lock().unwrap())
    }
    /// The key's input surrounded by temporary presses of `modifiers`.
    fn with_modifiers(key: INPUT, modifiers: u8, always_send_scancodes: bool) -> Vec<INPUT> {
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
    fn held(&self, keyboard: bool, id: u32, down: bool, owned: bool, input: INPUT) -> Result<()> {
        Self::update_held(
            &mut HELD.lock().unwrap(),
            (keyboard, id),
            down,
            owned,
            input,
            |inputs| self.send(inputs),
        )
    }
    fn update_held(
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
    pub fn feedback_allowed(&self, kind: u16) -> bool {
        !matches!(kind, 0x010b | 0x5500 | 0x5503) || (self.policy.forward_rumble && self.haptics)
    }
}
/// Inject input, following the input desktop when Windows refuses it: a UAC
/// prompt or the lock screen runs on the secure desktop. Returns how many
/// inputs Windows took, which it takes in order.
fn inject(inputs: &[INPUT]) -> usize {
    let send = |inputs: &[INPUT]| unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    let mut sent = send(inputs) as usize;
    if sent < inputs.len() && follow_input_desktop() {
        sent += send(&inputs[sent..]) as usize;
    }
    sent
}
struct InputDesktop {
    original: HDESK,
    current: HDESK,
}
impl Drop for InputDesktop {
    fn drop(&mut self) {
        unsafe {
            // Windows refuses to close a handle while a thread is attached to it.
            let _ = SetThreadDesktop(self.original);
            let _ = CloseDesktop(self.current);
        }
    }
}
thread_local! {
    static INPUT_DESKTOP: std::cell::RefCell<Option<InputDesktop>> = const { std::cell::RefCell::new(None) };
}

/// Move the calling thread to the desktop that receives input, which is the
/// secure desktop while a UAC prompt or the lock screen shows. Only a host
/// running as SYSTEM may attach to it; elsewhere this fails harmlessly.
/// Returns whether the thread is now on the input desktop.
pub fn follow_input_desktop() -> bool {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    INPUT_DESKTOP.with_borrow_mut(|attachment| unsafe {
        let Ok(desktop) = OpenInputDesktop(
            DF_ALLOWOTHERACCOUNTHOOK,
            false,
            DESKTOP_ACCESS_FLAGS(windows::Win32::Foundation::GENERIC_ALL.0),
        ) else {
            return false;
        };
        let original = GetThreadDesktop(GetCurrentThreadId());
        let Ok(original) = original else {
            let _ = CloseDesktop(desktop);
            return false;
        };
        if SetThreadDesktop(desktop).is_err() {
            let _ = CloseDesktop(desktop);
            return false;
        }
        if let Some(attached) = attachment {
            let previous = std::mem::replace(&mut attached.current, desktop);
            let _ = CloseDesktop(previous);
        } else {
            *attachment = Some(InputDesktop {
                original,
                current: desktop,
            });
        }
        true
    })
}
fn desktop_name(desktop: HDESK) -> Option<String> {
    let mut name = [0u16; 256];
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            size_of_val(&name) as u32,
            None,
        )
    }
    .ok()?;
    let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    Some(String::from_utf16_lossy(&name[..end]))
}
/// The desktop that receives input: "Default", or "Winlogon" while the lock
/// screen, the sign-in screen or a UAC prompt shows. None when this process
/// may not see it, as a host outside the service may not while Windows is locked.
pub fn input_desktop_name() -> Option<String> {
    unsafe {
        let desktop =
            OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS).ok()?;
        let name = desktop_name(desktop);
        let _ = CloseDesktop(desktop);
        name
    }
}
fn thread_desktop_name() -> Option<String> {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    // Windows owns the thread's desktop handle; it is not closed here.
    unsafe { GetThreadDesktop(GetCurrentThreadId()) }
        .ok()
        .and_then(desktop_name)
}
/// Whether Windows shows the lock screen, the sign-in screen or a UAC prompt.
pub fn secure_desktop_shown() -> bool {
    input_desktop_name().is_some_and(|name| !name.eq_ignore_ascii_case("default"))
}
/// Whether a thread on desktop `thread` must move to `input` before
/// configuring displays: Windows refuses QueryDisplayConfig and
/// SetDisplayConfig ("access denied") to a thread on another desktop. A
/// desktop that cannot be named keeps the work where it is.
fn must_follow(thread: Option<&str>, input: Option<&str>) -> bool {
    matches!((thread, input), (Some(thread), Some(input)) if !thread.eq_ignore_ascii_case(input))
}
/// For threads that keep a display's layout: follow the input desktop to the
/// lock screen and back, moving only when it changed.
pub fn keep_on_input_desktop() {
    if must_follow(
        thread_desktop_name().as_deref(),
        input_desktop_name().as_deref(),
    ) {
        follow_input_desktop();
    }
}
/// Run display configuration where Windows allows it. While the lock screen
/// or sign-in screen shows, that is only on its desktop, so the work runs on a
/// fresh thread attached there: a thread that ever owned a window, COM's
/// included, cannot change desktops. Otherwise it runs on the calling thread.
pub fn on_input_desktop<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    if !must_follow(
        thread_desktop_name().as_deref(),
        input_desktop_name().as_deref(),
    ) {
        return work();
    }
    let mut work = Some(work);
    let slot = &mut work;
    let done = std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .name("input-desktop".into())
            .spawn_scoped(scope, move || {
                if !follow_input_desktop() {
                    tracing::warn!("display work could not move to the input desktop");
                }
                slot.take().map(|work| work())
            });
        match worker {
            Ok(worker) => worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(error) => {
                tracing::warn!(%error, "display work could not start on the input desktop");
                None
            }
        }
    });
    match done {
        Some(done) => done,
        // The thread never started, so the work is still here.
        None => work.take().expect("unstarted display work")(),
    }
}
/// Inject touch or pen input, retrying once on the input desktop.
unsafe fn inject_pointer(
    device: HSYNTHETICPOINTERDEVICE,
    info: &[POINTER_TYPE_INFO],
) -> Result<()> {
    unsafe {
        match InjectSyntheticPointerInput(device, info) {
            Ok(()) => Ok(()),
            Err(_) if follow_input_desktop() => Ok(InjectSyntheticPointerInput(device, info)?),
            Err(error) => Err(error.into()),
        }
    }
}
// Moonlight 6.2 adds MODIFIER_EXTENDED (0x10) for keys such as keypad
// Enter that share a VK with a non-extended key. Keep this identity through
// held-key ownership, repeats and disconnect cleanup without changing the VK.
const MODIFIER_EXTENDED: u8 = 0x10;
const EXPLICIT_EXTENDED_KEY: u32 = 1 << 16;
fn legacy_extended_key(key: u16) -> bool {
    // Navigation, Insert/Delete, right Ctrl/Alt, Windows, Apps and keypad
    // divide, as in Sunshine. Print Screen (0x2c) is not: E0 54 maps to
    // nothing, so it never took a screenshot.
    matches!(
        key,
        0x21..=0x28 | 0x2d | 0x2e | 0xa3 | 0xa5 | 0x5b | 0x5c | 0x5d | 0x6f
    )
}
fn mapped_keyboard_identity(
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
fn is_modifier(key: u32) -> bool {
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
impl Drop for Injector {
    fn drop(&mut self) {
        if self.left_release.take().is_some() {
            let _ = self.held(false, 1, false, true, Self::button(1, false));
        }
        for key in &self.keys {
            let _ = self.held(
                true,
                *key,
                false,
                true,
                self.key_scan(*key, false, self.key_flags.get(key).copied().unwrap_or(0)),
            );
        }
        for button in &self.buttons {
            let _ = self.held(
                false,
                u32::from(*button),
                false,
                true,
                Self::button(*button, false),
            );
        }
        // Lift contacts that are still down before their devices go away,
        // as a client's cancel would. Best effort.
        let _ = self.cancel_touches();
        let _ = self.cancel_pen();
        unsafe {
            if let Some(device) = self.touch_device.take() {
                DestroySyntheticPointerDevice(device);
            }
            if let Some(device) = self.pen_device.take() {
                DestroySyntheticPointerDevice(device);
            }
        }
    }
}
fn pointer_event(p: &mut POINTER_INFO, event: u8, location: POINT) {
    let contact = p.pointerFlags & POINTER_FLAG_INCONTACT != POINTER_FLAG_NONE;
    p.pointerFlags = match event {
        0 => POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE,
        1 => {
            (if contact {
                POINTER_FLAG_UPDATE
            } else {
                POINTER_FLAG_DOWN
            }) | POINTER_FLAG_INRANGE
                | POINTER_FLAG_INCONTACT
        }
        2 => POINTER_FLAG_UP,
        3 => POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
        4 | 7 => {
            (if contact {
                POINTER_FLAG_UP
            } else {
                POINTER_FLAG_UPDATE
            }) | POINTER_FLAG_CANCELED
        }
        5 => p.pointerFlags | POINTER_FLAG_UPDATE,
        6 => POINTER_FLAG_UPDATE,
        _ => p.pointerFlags,
    };
    if matches!(event, 0 | 1 | 3) {
        p.ptPixelLocation = location;
    }
}
/// The pen's next frame from its last state, or None when there is nothing
/// to inject: Windows can pass a button only with an active pen.
#[allow(clippy::too_many_arguments)]
fn pen_frame(
    mut pen: POINTER_PEN_INFO,
    event: u8,
    tool: u8,
    buttons: u8,
    location: POINT,
    pressure: f32,
    rotation: u16,
    tilt: u8,
) -> Option<POINTER_PEN_INFO> {
    if event == 5 && pen.pointerInfo.pointerFlags == POINTER_FLAG_NONE {
        return None;
    }
    pen.pointerInfo.pointerType = PT_PEN;
    pen.pointerInfo.pointerId = 1;
    pointer_event(
        &mut pen.pointerInfo,
        if event == 7 { 4 } else { event },
        location,
    );
    pen.penFlags = if buttons != 0 {
        PEN_FLAG_BARREL
    } else {
        PEN_FLAG_NONE
    };
    if tool == 2 {
        pen.penFlags |= PEN_FLAG_ERASER | PEN_FLAG_INVERTED;
    }
    if event != 5 {
        // Windows has no hover distance, and a pressure of 0 is passed as
        // none, as in the C++ host.
        if pen.pointerInfo.pointerFlags & POINTER_FLAG_INCONTACT != POINTER_FLAG_NONE
            && pressure > 0.
        {
            pen.penMask = PEN_MASK_PRESSURE;
            pen.pressure = (pressure.clamp(0., 1.) * 1024.) as u32;
        } else {
            pen.penMask = PEN_MASK_NONE;
            pen.pressure = 0;
        }
        if rotation != u16::MAX {
            pen.penMask |= PEN_MASK_ROTATION;
            pen.rotation = u32::from(rotation % 360);
        }
        if tilt != u8::MAX && rotation != u16::MAX {
            let angle = f32::from(rotation).to_radians();
            let tilt = f32::from(tilt.min(90)).to_radians();
            pen.penMask |= PEN_MASK_TILT_X | PEN_MASK_TILT_Y;
            pen.tiltX = ((-angle).sin() * tilt.sin()).atan2(tilt.cos()).to_degrees() as i32;
            pen.tiltY = ((-angle).cos() * tilt.sin()).atan2(tilt.cos()).to_degrees() as i32;
        }
    }
    Some(pen)
}
/// The frame that lifts an active pen, as a client's cancel-all does.
fn pen_cancel(pen: POINTER_PEN_INFO) -> Option<POINTER_PEN_INFO> {
    if pen.pointerInfo.pointerFlags == POINTER_FLAG_NONE {
        return None;
    }
    pen_frame(pen, 7, 0, 0, POINT::default(), 0., u16::MAX, u8::MAX)
}
/// The pen after a frame: edge flags last one frame. A pen that ended, or
/// that Windows refused to put down and so never saw, is inactive whether or
/// not the frame went through; otherwise the refresh would inject the stale
/// frame again every 250 ms.
fn pen_after(mut pen: POINTER_PEN_INFO, event: u8, failed: bool) -> POINTER_PEN_INFO {
    let flags = pen.pointerInfo.pointerFlags;
    pen.pointerInfo.pointerFlags = if matches!(event, 2 | 4 | 6 | 7)
        || (failed && flags & POINTER_FLAG_DOWN != POINTER_FLAG_NONE)
    {
        POINTER_FLAG_NONE
    } else {
        (flags & !(POINTER_FLAG_DOWN | POINTER_FLAG_UP | POINTER_FLAG_CANCELED))
            | POINTER_FLAG_UPDATE
    };
    pen
}

#[cfg(test)]
#[path = "input/native_touch_tests.rs"]
mod native_touch_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an unlocked Windows desktop"]
    fn input_desktop_handles_close_on_reattachment_and_thread_exit() {
        use std::os::windows::process::CommandExt;
        use windows::Win32::System::Threading::*;
        if std::env::var_os("BUTTERPOLLO_DESKTOP_HANDLE_TEST").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "input::tests::input_desktop_handles_close_on_reattachment_and_thread_exit",
                    "--ignored",
                ])
                .env("BUTTERPOLLO_DESKTOP_HANDLE_TEST", "1")
                .creation_flags(CREATE_NO_WINDOW.0)
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let run = || {
            std::thread::spawn(|| {
                for _ in 0..8 {
                    assert!(follow_input_desktop());
                }
            })
            .join()
            .unwrap();
        };
        run();
        let handles = || {
            let mut count = 0;
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count).unwrap() };
            count
        };
        // A subprocess keeps unrelated parallel tests out of this measurement.
        let before = handles();
        for _ in 0..32 {
            run();
        }
        assert_eq!(handles(), before);
    }

    #[test]
    fn display_work_moves_only_to_a_different_named_input_desktop() {
        assert!(!must_follow(Some("Default"), Some("Default")));
        assert!(!must_follow(Some("Default"), Some("default")));
        assert!(must_follow(Some("Default"), Some("Winlogon")));
        assert!(must_follow(Some("Winlogon"), Some("Default")));
        // A host that may not open the lock screen's desktop stays put.
        assert!(!must_follow(Some("Default"), None));
        assert!(!must_follow(None, Some("Winlogon")));
    }

    #[test]
    fn display_work_returns_its_result_and_stays_on_the_caller_when_no_move_is_needed() {
        assert_eq!(on_input_desktop(|| 7), 7);
        let caller = std::thread::current().id();
        let ran_on = on_input_desktop(|| std::thread::current().id());
        if !must_follow(
            thread_desktop_name().as_deref(),
            input_desktop_name().as_deref(),
        ) {
            assert_eq!(ran_on, caller);
        }
        let borrowed = String::from("display");
        assert_eq!(on_input_desktop(|| borrowed.len()), 7);
    }

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
    fn vigem_ds4_keeps_primary_contact_slots_and_ignores_secondary_surfaces() {
        let mut pointers = BTreeMap::new();
        for pointer in [17, 18] {
            submit_touch(&mut pointers, VIGEM_DS4, 2, &touch(2, 0, 1, pointer)).unwrap();
        }
        assert!(submit_touch(&mut pointers, VIGEM_DS4, 2, &touch(2, 0, 1, 19)).is_none());
        assert!(submit_touch(&mut pointers, VIGEM_DS4, 2, &touch(2, 1, 7, 17)).is_none());
        assert_eq!(pointers.len(), 2);
        submit_touch(&mut pointers, VIGEM_DS4, 2, &touch(2, 0, 7, 17)).unwrap();
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
        assert!(submit_touch_on(&mut pointers, VIGEM_DS4, 2, &touch(2, 1, 1, 0), false).is_none());
        let full = submit_touch_on(&mut pointers, VIGEM_DS4, 2, &touch(2, 0, 1, 0), false).unwrap();
        // A single touchpad keeps the whole surface.
        assert_eq!(&full[14..16], &(16383u16).to_le_bytes());
        assert_eq!(pointers.len(), 1);
        // Pads without a touch surface take no touch even when announced dual.
        for profile in [3, 4, 7, VIGEM_X360] {
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

    fn keyboard(key: u32, down: bool, flags: u8) -> KEYBDINPUT {
        // No input injection or layout API calls: normalized scan codes come
        // from the static US table; non-normalized keys retain their VK.
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
    fn input_starts_before_the_stream_display_exists_and_ignores_absolute_input() {
        // Keyboard and controller input, including a controller's one-time
        // arrival, needs an injector before the stream's display exists.
        let mut injector = Injector::new(r"\\.\DISPLAY99", "vhf").unwrap();
        // Nothing below may reach Windows: there is no display to map onto.
        assert!(injector.rect.is_none());
        for event in [
            Event::Absolute {
                x: 10,
                y: 20,
                width: 100,
                height: 100,
            },
            Event::Touch {
                event: 1,
                id: 7,
                x: 0.5,
                y: 0.5,
                pressure: 0.5,
                major: 0.1,
                minor: 0.1,
                rotation: u16::MAX,
            },
            Event::Pen {
                event: 1,
                tool: 1,
                buttons: 0,
                x: 0.5,
                y: 0.5,
                pressure: 0.5,
                rotation: u16::MAX,
                tilt: u8::MAX,
            },
        ] {
            injector.apply(&event).unwrap();
        }
        assert!(injector.absolute);
        assert!(injector.touches.is_empty());
        assert!(injector.touch_device.is_none() && injector.pen_device.is_none());
        assert_eq!(injector.pen.pointerInfo.pointerFlags, POINTER_FLAG_NONE);
        // The session's display is followed while it is still missing.
        injector.set_output(r"\\.\DISPLAY98");
        assert!(injector.rect.is_none());
        assert_eq!(injector.output, r"\\.\DISPLAY98");
        assert_eq!(injector.lookup.as_ref().unwrap().output, r"\\.\DISPLAY98");
    }

    thread_local! {
        static CALLS: std::cell::RefCell<Vec<Vec<INPUT>>> = const { std::cell::RefCell::new(Vec::new()) };
        /// How many inputs of a call Windows takes.
        static TAKES: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
    }
    /// SendInput's stand-in: nothing reaches the desktop.
    fn record(inputs: &[INPUT]) -> usize {
        CALLS.with_borrow_mut(|calls| calls.push(inputs.to_vec()));
        TAKES.get().min(inputs.len())
    }
    fn recording() -> Injector {
        let mut injector = Injector::new(r"\\.\DISPLAY99", "vhf").unwrap();
        injector.inject = record;
        injector
    }
    fn describe(input: &INPUT) -> String {
        unsafe {
            if input.r#type == INPUT_KEYBOARD {
                let ki = input.Anonymous.ki;
                format!("key {} {} {:#x}", ki.wVk.0, ki.wScan, ki.dwFlags.0)
            } else {
                let mi = input.Anonymous.mi;
                format!(
                    "mouse {} {} {} {:#x}",
                    mi.dx, mi.dy, mi.mouseData, mi.dwFlags.0
                )
            }
        }
    }
    /// The SendInput calls made since the last look.
    fn calls() -> Vec<Vec<String>> {
        CALLS
            .with_borrow_mut(std::mem::take)
            .iter()
            .map(|call| call.iter().map(describe).collect())
            .collect()
    }
    fn described(inputs: &[INPUT]) -> Vec<String> {
        inputs.iter().map(describe).collect()
    }
    fn key(key: u16, down: bool) -> Event {
        Event::Keyboard {
            key,
            modifiers: 0,
            flags: 0,
            down,
        }
    }
    fn click(button: u8, down: bool) -> Event {
        Event::MouseButton { button, down }
    }
    // HELD is shared by every injector: each test below uses its own keys
    // and buttons.

    #[test]
    fn a_pass_of_moves_keys_buttons_and_text_is_one_call_in_order() {
        let mut injector = recording();
        let events = [
            Event::Relative { x: 1, y: 2 },
            key(0x42, true),
            click(2, true),
            Event::Scroll {
                amount: 120,
                horizontal: false,
            },
            Event::Text("hi".into()),
        ];
        assert!(injector.apply_all(&events).is_empty());
        assert_eq!(
            calls(),
            [described(&[
                Injector::mouse(1, 2, 0, MOUSEEVENTF_MOVE),
                injector.key_scan(0x42, true, 0),
                Injector::button(2, true),
                Injector::mouse(0, 0, 120, MOUSEEVENTF_WHEEL),
                Injector::key(u16::from(b'h'), true, true),
                Injector::key(u16::from(b'h'), false, true),
                Injector::key(u16::from(b'i'), true, true),
                Injector::key(u16::from(b'i'), false, true),
            ])]
        );
        assert!(injector.keys.contains(&0x42) && injector.buttons.contains(&2));
        // Their releases in the next pass also share a call.
        assert!(
            injector
                .apply_all(&[key(0x42, false), click(2, false)])
                .is_empty()
        );
        assert_eq!(
            calls(),
            [described(&[
                injector.key_scan(0x42, false, 0),
                Injector::button(2, false),
            ])]
        );
        assert!(injector.keys.is_empty() && injector.buttons.is_empty());
        let held = HELD.lock().unwrap();
        assert!(!held.contains_key(&(true, 0x42)) && !held.contains_key(&(false, 2)));
    }

    #[test]
    fn a_press_and_its_release_in_one_pass_both_go_out_in_order() {
        let mut injector = recording();
        // A release goes out only for a key the client holds, and a press
        // is held once Windows took it: the press is sent first.
        let events = [
            key(0x43, true),
            key(0x43, false),
            click(3, true),
            click(3, false),
        ];
        assert!(injector.apply_all(&events).is_empty());
        assert_eq!(
            calls(),
            [
                described(&[injector.key_scan(0x43, true, 0)]),
                described(&[injector.key_scan(0x43, false, 0), Injector::button(3, true)]),
                described(&[Injector::button(3, false)]),
            ]
        );
        assert!(injector.keys.is_empty() && injector.buttons.is_empty());
        assert!(injector.repeat.is_none());
    }

    #[test]
    fn presses_windows_refuses_are_not_held_and_refused_releases_are_forgotten() {
        let mut injector = recording();
        TAKES.set(1);
        let errors = injector.apply_all(&[
            Event::Relative { x: 0, y: 0 },
            key(0x44, true),
            click(4, true),
        ]);
        assert_eq!(errors.len(), 2);
        assert!(injector.keys.is_empty() && injector.buttons.is_empty());
        assert!(injector.repeat.is_none());
        {
            let held = HELD.lock().unwrap();
            assert!(!held.contains_key(&(true, 0x44)) && !held.contains_key(&(false, 4)));
        }
        TAKES.set(usize::MAX);
        assert!(injector.apply_all(&[key(0x45, true)]).is_empty());
        assert!(injector.keys.contains(&0x45));
        TAKES.set(0);
        assert_eq!(injector.apply_all(&[key(0x45, false)]).len(), 1);
        assert!(injector.keys.is_empty() && injector.repeat.is_none());
        assert!(!HELD.lock().unwrap().contains_key(&(true, 0x45)));
        assert_eq!(calls().len(), 3);
    }

    /// An injector for the first display, if this machine has one.
    fn on_first_display() -> Option<(Injector, String)> {
        let name = crate::capture::displays()
            .ok()?
            .first()?
            .display_name
            .clone();
        let mut injector = Injector::new(&name, "vhf").unwrap();
        injector.inject = record;
        Some((injector, name))
    }

    #[test]
    fn absolute_input_waits_for_the_display_lookup_it_needs() {
        let Some((mut injector, _)) = on_first_display() else {
            return;
        };
        // Created at once; the display is looked up on another thread.
        assert!(injector.rect.is_none());
        assert!(injector.lookup.as_ref().is_some_and(|l| l.thread.is_some()));
        let events = [Event::Absolute {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        }];
        assert!(injector.apply_all(&events).is_empty());
        assert!(injector.rect.is_some());
        assert_eq!(calls().len(), 1);
    }

    #[test]
    fn returning_to_the_current_display_discards_an_obsolete_lookup() {
        let mut injector = recording();
        injector.settle_lookup(true);
        let output = injector.output.clone();
        let current = RECT {
            left: 0,
            top: 0,
            right: 100,
            bottom: 100,
        };
        injector.rect = Some(current);
        injector.lookup = Some(Lookup {
            output: r"\\.\DISPLAY98".into(),
            started: std::time::Instant::now(),
            thread: Some(std::thread::spawn(|| {
                Ok(RECT {
                    left: 100,
                    top: 0,
                    right: 200,
                    bottom: 100,
                })
            })),
        });
        injector.set_output(&output);
        assert_eq!(injector.display(), Some(current));
        assert_eq!(injector.output, output);
    }

    #[test]
    fn a_renamed_display_is_looked_up_without_holding_up_the_pass() {
        let Some((mut injector, first)) = on_first_display() else {
            return;
        };
        injector.settle_lookup(true);
        let rect = injector.rect.unwrap();
        // The session names a display that does not exist (yet): absolute
        // input keeps the display it has until the new one is found.
        injector.set_output(r"\\.\DISPLAY99");
        let lookup = injector.lookup.as_ref().unwrap();
        assert!(lookup.thread.is_some() && lookup.output == r"\\.\DISPLAY99");
        assert_eq!(injector.output, first);
        let started = lookup.started;
        injector.settle_lookup(true);
        assert_eq!(
            (injector.rect, injector.output.as_str()),
            (Some(rect), &*first)
        );
        // A name not found is not looked up again on every pass.
        injector.set_output(r"\\.\DISPLAY99");
        if started.elapsed() < RECT_INTERVAL {
            assert!(injector.lookup.as_ref().unwrap().thread.is_none());
        }
    }

    #[test]
    fn touch_and_pen_keep_their_place_between_mouse_moves() {
        let mut injector = recording();
        // There is no display: touch and pen reach no one, but still split
        // the pass so mouse input around them keeps its order.
        let events = [
            Event::Relative { x: 1, y: 0 },
            Event::Touch {
                event: 1,
                id: 7,
                x: 0.5,
                y: 0.5,
                pressure: 0.5,
                major: 0.,
                minor: 0.,
                rotation: u16::MAX,
            },
            Event::Relative { x: 2, y: 0 },
            Event::Pen {
                event: 0,
                tool: 1,
                buttons: 0,
                x: 0.5,
                y: 0.5,
                pressure: 0.,
                rotation: u16::MAX,
                tilt: u8::MAX,
            },
            Event::Relative { x: 3, y: 0 },
        ];
        assert!(injector.apply_all(&events).is_empty());
        assert_eq!(
            calls(),
            [1, 2, 3].map(|x| described(&[Injector::mouse(x, 0, 0, MOUSEEVENTF_MOVE)]))
        );
    }

    #[test]
    fn a_held_touch_is_repeated_while_pen_events_keep_coming() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        // A touch is held still while the pen hovers, sending an event every
        // 8 ms (each injection restarts the pen's period), for one second.
        let mut touch = start;
        let mut repeats = vec![];
        for ms in (8..=1000).step_by(8) {
            let now = start + Duration::from_millis(ms);
            let mut pen = now;
            assert!(!refresh_due(&mut pen, now));
            if refresh_due(&mut touch, now) {
                repeats.push(ms);
            }
        }
        assert_eq!(repeats, [256, 512, 768]);
        // And a hovering pen is repeated while touch events keep coming.
        let mut pen = start;
        let mut repeats = 0;
        for ms in (8..=1000).step_by(8) {
            let now = start + Duration::from_millis(ms);
            let mut touch = now;
            assert!(!refresh_due(&mut touch, now));
            repeats += usize::from(refresh_due(&mut pen, now));
        }
        assert_eq!(repeats, 3);
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

    fn pen_event(pen: POINTER_PEN_INFO, event: u8, pressure: f32) -> Option<POINTER_PEN_INFO> {
        let location = POINT { x: 10, y: 20 };
        pen_frame(pen, event, 1, 0, location, pressure, u16::MAX, u8::MAX)
    }

    #[test]
    fn pen_pressure_and_button_frames_match_the_cpp_host() {
        let idle = POINTER_PEN_INFO::default();
        let in_contact = POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT;
        // A button change with no active pen injects nothing.
        assert!(pen_frame(idle, 5, 1, 1, POINT::default(), 0., u16::MAX, u8::MAX).is_none());
        // A pressure of 0 in contact is passed as none.
        let down = pen_event(idle, 1, 0.).unwrap();
        assert_eq!(
            down.pointerInfo.pointerFlags,
            POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT
        );
        assert_eq!((down.penMask, down.pressure), (PEN_MASK_NONE, 0));
        let held = pen_after(down, 1, false);
        assert_eq!(held.pointerInfo.pointerFlags, in_contact);
        let moved = pen_event(held, 3, 0.).unwrap();
        assert_eq!((moved.penMask, moved.pressure), (PEN_MASK_NONE, 0));
        let pressed = pen_event(held, 3, 0.5).unwrap();
        assert_eq!(
            (pressed.penMask, pressed.pressure),
            (PEN_MASK_PRESSURE, 512)
        );
        // A hover's distance is never passed as pressure.
        let hover = pen_event(idle, 0, 0.5).unwrap();
        assert_eq!((hover.penMask, hover.pressure), (PEN_MASK_NONE, 0));
        // With an active pen, a button change is passed on.
        let hovering = pen_after(hover, 0, false);
        let barrel = pen_frame(hovering, 5, 1, 1, POINT::default(), 0., u16::MAX, u8::MAX).unwrap();
        assert_eq!(barrel.penFlags, PEN_FLAG_BARREL);
        assert_eq!(
            barrel.pointerInfo.pointerFlags,
            POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE
        );
        assert_eq!(barrel.pointerInfo.ptPixelLocation, POINT { x: 10, y: 20 });
    }

    #[test]
    fn a_pen_frame_windows_refuses_is_not_repeated_as_a_stale_frame() {
        let in_contact = POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT;
        let down = pen_event(POINTER_PEN_INFO::default(), 1, 0.5).unwrap();
        // Windows never saw the pen go down: there is nothing to repeat.
        assert_eq!(
            pen_after(down, 1, true).pointerInfo.pointerFlags,
            POINTER_FLAG_NONE
        );
        let held = pen_after(down, 1, false);
        // An ended pen is inactive whether or not its last frame went through.
        for event in [2, 4, 6, 7] {
            for failed in [false, true] {
                let frame = pen_event(held, event, 0.).unwrap();
                assert_eq!(
                    pen_after(frame, event, failed).pointerInfo.pointerFlags,
                    POINTER_FLAG_NONE
                );
            }
        }
        // A refused move keeps the pen's state, without edge flags.
        let moved = pen_event(held, 3, 0.5).unwrap();
        assert_eq!(
            pen_after(moved, 3, true).pointerInfo.pointerFlags,
            in_contact
        );
    }

    #[test]
    fn an_active_pen_is_lifted_before_its_device_goes_away() {
        let idle = POINTER_PEN_INFO::default();
        assert!(pen_cancel(idle).is_none());
        let held = pen_after(pen_event(idle, 1, 0.5).unwrap(), 1, false);
        let lifted = pen_cancel(held).unwrap();
        assert_eq!(
            lifted.pointerInfo.pointerFlags,
            POINTER_FLAG_UP | POINTER_FLAG_CANCELED
        );
        assert_eq!((lifted.penMask, lifted.pressure), (PEN_MASK_NONE, 0));
        assert_eq!(
            pen_after(lifted, 7, true).pointerInfo.pointerFlags,
            POINTER_FLAG_NONE
        );
        let hovering = pen_after(pen_event(idle, 0, 0.).unwrap(), 0, false);
        assert_eq!(
            pen_cancel(hovering).unwrap().pointerInfo.pointerFlags,
            POINTER_FLAG_UPDATE | POINTER_FLAG_CANCELED
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
