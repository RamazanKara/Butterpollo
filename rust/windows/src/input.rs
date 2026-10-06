use anyhow::{Result, bail};
use butterpollo_core::input::Input as Event;
use std::{
    collections::{BTreeMap, BTreeSet},
    mem::size_of,
};
use windows::{
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::*,
        Storage::FileSystem::*,
        System::IO::DeviceIoControl,
        UI::{
            Controls::*,
            Input::{KeyboardAndMouse::*, Pointer::*},
            WindowsAndMessaging::*,
        },
    },
    core::{GUID, PCWSTR},
};

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
    handle: HANDLE,
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
        // no separate host bit for dual touchpads. Only pad 0 is supported by
        // the current virtual gamepad protocol; apply() filters other pads.
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
        let handle = open_interface(GUID::from_u128(0x27debbf5_1d1e_4e9c_906d_d104b1418b2b))?;
        let mut g = Self {
            handle,
            active: BTreeMap::new(),
            profile,
            available: 0,
            profiles: BTreeMap::new(),
            arrivals: BTreeMap::new(),
            states: BTreeMap::new(),
            policy,
            started: std::time::Instant::now(),
            pointers: BTreeMap::new(),
            unsupported_touchpads: BTreeSet::new(),
            last_feedback: BTreeMap::new(),
        };
        let out = g.ioctl(0x800, &request(8, None), 28)?;
        if out.len() != 28 || u16::from_le_bytes(out[4..6].try_into().unwrap()) != 2 {
            bail!("incompatible VHF gamepad protocol");
        }
        g.available = u32::from_le_bytes(out[12..16].try_into().unwrap());
        if profile != 0 && g.available & (1 << (profile - 1)) == 0 {
            bail!("configured controller profile is unavailable in the installed VHF driver");
        }
        Ok(g)
    }
    fn ioctl(&mut self, function: u32, data: &[u8], output: usize) -> Result<Vec<u8>> {
        unsafe {
            let mut result = vec![0; output];
            let mut n = 0;
            DeviceIoControl(
                self.handle,
                (0x22 << 16) | (3 << 14) | (function << 2),
                Some(data.as_ptr() as *const _),
                data.len() as u32,
                if output == 0 {
                    None
                } else {
                    Some(result.as_mut_ptr() as *mut _)
                },
                output as u32,
                Some(&mut n),
                None,
            )?;
            result.truncate(n as usize);
            Ok(result)
        }
    }
    fn ensure(&mut self, id: u16) -> Result<()> {
        if id >= 16 {
            bail!("controller ID out of range");
        }
        let (kind, capabilities) = self.arrivals.get(&id).copied().unwrap_or_default();
        let profile = self
            .policy
            .controller_profile(self.profile, kind, capabilities, self.available)
            .ok_or_else(|| anyhow::anyhow!("VHF driver has no supported controller profile"))?;
        if let Some(global) = self.active.get(&id).copied() {
            if self.profiles.get(&id) == Some(&profile) {
                return Ok(());
            }
            // Some clients send arrival capabilities after their first state.
            self.ioctl(0x802, &request(12, Some(u32::from(global))), 0)?;
            self.active.remove(&id);
            self.profiles.remove(&id);
            self.pointers.retain(|(pad, _), _| u16::from(*pad) != id);
            SLOTS.lock().unwrap()[global as usize] = false;
        }
        let mut slots = SLOTS.lock().unwrap();
        for global in 0..16u16 {
            if slots[global as usize] {
                continue;
            }
            let mut b = request(16, Some(u32::from(global)));
            b.extend_from_slice(&profile.to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
            if self.ioctl(0x801, &b, 0).is_ok() {
                slots[global as usize] = true;
                self.active.insert(id, global);
                self.profiles.insert(id, profile);
                tracing::info!(
                    controller = id,
                    client_type = kind,
                    capabilities = format!("{capabilities:#x}"),
                    profile = profile_name(profile),
                    "virtual controller connected"
                );
                return Ok(());
            }
        }
        bail!("no free VHF controller slots")
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
                        self.ioctl(0x802, &request(12, Some(u32::from(global))), 0)?;
                        self.active.remove(&i);
                        self.profiles.remove(&i);
                        self.arrivals.remove(&i);
                        self.states.remove(&i);
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
                let mut b = request(28, Some(u32::from(self.active[&u16::from(*id)])));
                b.extend_from_slice(&[*kind, 0, 0, 0]);
                for f in xyz {
                    b.extend_from_slice(
                        &((*f as f64 * 1000.).clamp(i32::MIN as f64, i32::MAX as f64) as i32)
                            .to_le_bytes(),
                    );
                }
                self.ioctl(0x806, &b, 0)?;
            }
            Event::ControllerTouch { id, touchpad, .. } => {
                // Protocol 2 exposes one PlayStation touch surface with two
                // contacts, not two surfaces. Never reinterpret pad 1 as pad 0.
                if *touchpad != 0 {
                    if self.unsupported_touchpads.insert(*id) {
                        tracing::warn!(
                            controller = id,
                            touchpad,
                            "controller touchpad ignored: the virtual gamepad driver supports only touchpad 0"
                        );
                    }
                    return Ok(());
                }
                if !self.active.contains_key(&u16::from(*id)) {
                    return Ok(());
                }
                if let Some(b) = gamepad_touch_request(
                    &self.pointers,
                    self.profiles[&u16::from(*id)],
                    self.active[&u16::from(*id)],
                    event,
                ) {
                    self.ioctl(0x805, &b.packet, 0)?;
                    b.submitted(&mut self.pointers);
                }
            }
            Event::Battery { id, state, percent } => {
                // Only the PlayStation and Switch profiles have a battery.
                if !self.motion_supported(u16::from(*id)) {
                    return Ok(());
                }
                let mut b = request(16, Some(u32::from(self.active[&u16::from(*id)])));
                b.extend_from_slice(&[*percent, *state, 0, 0]);
                self.ioctl(0x807, &b, 0)?;
            }
            _ => {}
        }
        Ok(())
    }
    /// New feedback (rumble, lights, trigger effects) for each controller.
    /// A controller with nothing pending or a failed poll does not hide the
    /// others' feedback, and a repeated report is not sent again.
    pub fn feedback(&mut self) -> Result<Vec<(u16, u16, Vec<u8>)>> {
        let mut output = vec![];
        for (id, global) in self.active.clone() {
            let Ok(b) = self.ioctl(0x804, &request(12, Some(u32::from(global))), 48) else {
                continue;
            };
            if b.len() == 48 {
                let kind = u16::from_le_bytes(b[12..14].try_into().unwrap());
                let len = u16::from_le_bytes(b[14..16].try_into().unwrap()) as usize;
                if kind != 0 && len <= 32 {
                    let report = (kind, b[16..16 + len].to_vec());
                    if self.last_feedback.get(&id) != Some(&report) {
                        self.last_feedback.insert(id, report.clone());
                        output.push((id, report.0, report.1));
                    }
                }
            }
        }
        self.last_feedback
            .retain(|id, _| self.active.contains_key(id));
        Ok(output)
    }
    fn submit(
        &mut self,
        id: u16,
        buttons: u32,
        left: u8,
        right: u8,
        sticks: &[i16; 4],
    ) -> Result<()> {
        let mut b = request(28, Some(u32::from(self.active[&id])));
        b.extend_from_slice(&buttons.to_le_bytes());
        for stick in sticks {
            b.extend_from_slice(&stick.to_le_bytes());
        }
        b.extend_from_slice(&[left, right, 0, 0]);
        self.ioctl(0x803, &b, 0)?;
        Ok(())
    }
    fn refresh(&mut self) -> Result<()> {
        let mut updates = Vec::new();
        for (id, (event, back)) in &mut self.states {
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
        matches!(self.profiles.get(&id), Some(5..=7))
    }
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
struct GamepadTouchRequest {
    packet: Vec<u8>,
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
fn gamepad_touch_request(
    pointers: &BTreeMap<(u8, u32), u8>,
    profile: u16,
    global: u16,
    input: &Event,
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
    if *touchpad != 0 || !matches!(profile, 5 | 6) {
        return None;
    }
    let event = match event {
        0..=4 => *event,
        6 => 4,
        7 => 5,
        _ => return None,
    };
    let key = (*id, *pointer);
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
    for f in [*x, *y, *pressure] {
        b.extend_from_slice(&((f.clamp(0., 1.) * 65535.) as u16).to_le_bytes());
    }
    b.extend_from_slice(&[0, 0]);
    Some(GamepadTouchRequest {
        packet: b,
        id: *id,
        pointer: *pointer,
        slot,
        event,
    })
}
impl Drop for Gamepads {
    fn drop(&mut self) {
        for (_, global) in self.active.clone() {
            let _ = self.ioctl(0x802, &request(12, Some(u32::from(global))), 0);
            SLOTS.lock().unwrap()[global as usize] = false;
        }
        unsafe {
            let _ = CloseHandle(self.handle);
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
    profile: u16,
    refreshed: std::time::Instant,
    pub gamepads: Option<Gamepads>,
    rect: RECT,
    policy: butterpollo_core::input_policy::Policy,
    key_flags: BTreeMap<u32, u8>,
    /// Repeating key, its flags, the modifiers it adds and when it repeats.
    repeat: Option<(u32, u8, u8, std::time::Instant)>,
    scroll: [i32; 2],
    haptics: bool,
    /// The display absolute input maps onto, and when its rectangle was read.
    output: String,
    rect_read: std::time::Instant,
    /// When to try the virtual gamepad driver again after it failed to open.
    gamepad_retry: Option<std::time::Instant>,
    /// Whether the client last moved the mouse by absolute position, and a
    /// left-button release held back meanwhile.
    absolute: bool,
    left_release: Option<std::time::Instant>,
}
/// How long a left release waits after absolute input, as in Sunshine.
const LEFT_RELEASE_DELAY: std::time::Duration = std::time::Duration::from_millis(10);
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
        let rect = display_rect(output)?;
        Ok(Self {
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            touches: BTreeMap::new(),
            touch_device: None,
            pen_device: None,
            pen: Default::default(),
            profile: match profile {
                "vhf_xbox" => 4,
                "vhf_xbox_one" | "x360" => 3,
                "vhf_ds4" | "ds4" => 5,
                "vhf_ds5" | "ds5" => 6,
                "vhf_switch" => 7,
                "vhf" => 0,
                other => {
                    butterpollo_core::config::fallback("gamepad", other, "auto");
                    0
                }
            },
            refreshed: std::time::Instant::now(),
            gamepads: None,
            policy: butterpollo_core::input_policy::Policy::resolve(config)?,
            key_flags: BTreeMap::new(),
            repeat: None,
            scroll: [0; 2],
            haptics: true,
            rect,
            output: output.to_owned(),
            rect_read: std::time::Instant::now(),
            gamepad_retry: None,
            absolute: false,
            left_release: None,
        })
    }
    /// Map absolute input onto `output` from now on: the stream's display can
    /// be created, recreated or renamed after input began.
    pub fn set_output(&mut self, output: &str) {
        if output.is_empty() || output.eq_ignore_ascii_case(&self.output) {
            return;
        }
        match display_rect(output) {
            Ok(rect) => {
                self.rect = rect;
                self.output = output.to_owned();
                self.rect_read = std::time::Instant::now();
            }
            Err(error) => tracing::debug!(%error, output, "input display unavailable"),
        }
    }
    /// Inject input, following the input desktop when Windows refuses it:
    /// a UAC prompt or the lock screen runs on the secure desktop.
    fn send(inputs: &[INPUT]) -> Result<()> {
        let sent =
            || unsafe { SendInput(inputs, size_of::<INPUT>() as i32) } == inputs.len() as u32;
        if !sent() && !(follow_input_desktop() && sent()) {
            bail!(
                "Windows input injection failed: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
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
    fn location(&self, x: f32, y: f32) -> POINT {
        POINT {
            x: self.rect.left
                + ((self.rect.right - self.rect.left - 1).max(0) as f32 * x.clamp(0., 1.)) as i32,
            y: self.rect.top
                + ((self.rect.bottom - self.rect.top - 1).max(0) as f32 * y.clamp(0., 1.)) as i32,
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
        unsafe {
            if self.touch_device.is_none() {
                self.touch_device = Some(CreateSyntheticPointerDevice(
                    PT_TOUCH,
                    32,
                    POINTER_FEEDBACK_NONE,
                )?);
            }
            if event == 7 {
                for p in self.touches.values_mut() {
                    pointer_event(&mut p.pointerInfo, 4, POINT::default());
                }
                let result = self.inject_touches();
                self.touches.clear();
                return result;
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
            pointer_event(&mut p.pointerInfo, event, self.location(x, y));
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
                        let width = ((angle.cos().abs() * major + angle.sin().abs() * minor)
                            * (self.rect.right - self.rect.left) as f32)
                            .max(1.);
                        let height = ((angle.sin().abs() * major + angle.cos().abs() * minor)
                            * (self.rect.bottom - self.rect.top) as f32)
                            .max(1.);
                        let center = p.pointerInfo.ptPixelLocation;
                        p.rcContact = RECT {
                            left: (center.x - (width / 2.).ceil() as i32).max(self.rect.left),
                            right: (center.x + (width / 2.).ceil() as i32).min(self.rect.right),
                            top: (center.y - (height / 2.).ceil() as i32).max(self.rect.top),
                            bottom: (center.y + (height / 2.).ceil() as i32).min(self.rect.bottom),
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
            self.refreshed = std::time::Instant::now();
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
        unsafe {
            if self.pen_device.is_none() {
                self.pen_device = Some(CreateSyntheticPointerDevice(
                    PT_PEN,
                    1,
                    POINTER_FEEDBACK_NONE,
                )?);
            }
            let location = self.location(x, y);
            self.pen.pointerInfo.pointerType = PT_PEN;
            self.pen.pointerInfo.pointerId = 1;
            pointer_event(
                &mut self.pen.pointerInfo,
                if event == 7 { 4 } else { event },
                location,
            );
            self.pen.penFlags = if buttons != 0 {
                PEN_FLAG_BARREL
            } else {
                PEN_FLAG_NONE
            };
            if tool == 2 {
                self.pen.penFlags |= PEN_FLAG_ERASER | PEN_FLAG_INVERTED;
            }
            if event != 5 {
                self.pen.penMask = PEN_MASK_PRESSURE;
                self.pen.pressure = if self.pen.pointerInfo.pointerFlags & POINTER_FLAG_INCONTACT
                    != POINTER_FLAG_NONE
                {
                    (pressure.clamp(0., 1.) * 1024.) as u32
                } else {
                    0
                };
                if rotation != u16::MAX {
                    self.pen.penMask |= PEN_MASK_ROTATION;
                    self.pen.rotation = u32::from(rotation % 360);
                }
                if tilt != u8::MAX && rotation != u16::MAX {
                    let angle = f32::from(rotation).to_radians();
                    let tilt = f32::from(tilt.min(90)).to_radians();
                    self.pen.penMask |= PEN_MASK_TILT_X | PEN_MASK_TILT_Y;
                    self.pen.tiltX =
                        ((-angle).sin() * tilt.sin()).atan2(tilt.cos()).to_degrees() as i32;
                    self.pen.tiltY =
                        ((-angle).cos() * tilt.sin()).atan2(tilt.cos()).to_degrees() as i32;
                }
            }
            inject_pointer(
                self.pen_device.unwrap(),
                &[POINTER_TYPE_INFO {
                    r#type: PT_PEN,
                    Anonymous: POINTER_TYPE_INFO_0 { penInfo: self.pen },
                }],
            )?;
            self.pen.pointerInfo.pointerFlags &=
                !(POINTER_FLAG_DOWN | POINTER_FLAG_UP | POINTER_FLAG_CANCELED);
            if matches!(event, 2 | 4 | 6 | 7) {
                self.pen.pointerInfo.pointerFlags = POINTER_FLAG_NONE;
            } else {
                self.pen.pointerInfo.pointerFlags |= POINTER_FLAG_UPDATE;
            }
            self.refreshed = std::time::Instant::now();
        }
        Ok(())
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
        if let Some(gamepads) = &mut self.gamepads {
            keep(gamepads.refresh());
        }
        let now = std::time::Instant::now();
        if self.left_release.is_some_and(|due| now >= due) {
            self.left_release = None;
            keep(Self::held(false, 1, false, true, Self::button(1, false)));
        }
        if let Some((key, flags, modifiers, due)) = self.repeat
            && now >= due
        {
            self.repeat = Some((key, flags, modifiers, now + self.policy.repeat_period));
            keep(Self::send(
                &self.with_modifiers(self.key_scan(key, true, flags), modifiers),
            ));
        }
        // A game can change its display's resolution or position mid-stream.
        if self.rect_read.elapsed() >= std::time::Duration::from_millis(500) {
            self.rect_read = now;
            if let Some(rect) = current_rect(&self.output) {
                self.rect = rect;
            }
        }
        if self.refreshed.elapsed() >= std::time::Duration::from_millis(250) {
            self.refreshed = now;
            keep(self.inject_touches());
            if self.pen.pointerInfo.pointerFlags != POINTER_FLAG_NONE
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
        }
        first.map_or(Ok(()), Err)
    }
    pub fn apply(&mut self, e: &Event) -> Result<()> {
        if !self.policy.allows(e) {
            return Ok(());
        }
        use Event::*;
        match e {
            Relative { x, y } => {
                self.absolute = false;
                Self::send(&[Self::mouse(
                    i32::from(*x),
                    i32::from(*y),
                    0,
                    MOUSEEVENTF_MOVE,
                )])?
            }
            Absolute {
                x,
                y,
                width,
                height,
            } => unsafe {
                self.absolute = true;
                let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
                let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
                let w = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
                let h = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
                // The client's far edge is the display's last pixel, not the
                // first pixel of its neighbour.
                let (width, height) = (i64::from(*width).max(1), i64::from(*height).max(1));
                let px = self.rect.left
                    + (i64::from(*x).clamp(0, width)
                        * i64::from((self.rect.right - self.rect.left - 1).max(0))
                        / width) as i32;
                let py = self.rect.top
                    + (i64::from(*y).clamp(0, height)
                        * i64::from((self.rect.bottom - self.rect.top - 1).max(0))
                        / height) as i32;
                Self::send(&[Self::mouse(
                    ((i64::from(px - left) * 65535) / i64::from((w - 1).max(1))) as i32,
                    ((i64::from(py - top) * 65535) / i64::from((h - 1).max(1))) as i32,
                    0,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                )])?;
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
                        return Ok(());
                    }
                    if *down && self.left_release.take().is_some() {
                        // Still down in Windows: its release never went out.
                        self.buttons.insert(*button);
                        return Ok(());
                    }
                }
                if *button == 3 && *down && self.left_release.is_some() {
                    return Self::send(&[Self::button(3, true), Self::button(3, false)]);
                }
                let owned = self.buttons.contains(button);
                let sent = Self::held(
                    false,
                    u32::from(*button),
                    *down,
                    owned,
                    Self::button(*button, *down),
                );
                // A release is recorded even when Windows refused it.
                if *down {
                    sent?;
                    self.buttons.insert(*button);
                } else {
                    self.buttons.remove(button);
                    sent?;
                }
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
                    return Ok(());
                }
                Self::send(&[Self::mouse(
                    0,
                    0,
                    amount as u32,
                    if *horizontal {
                        MOUSEEVENTF_HWHEEL
                    } else {
                        MOUSEEVENTF_WHEEL
                    },
                )])?;
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
                let sent = if modifiers == 0 {
                    Self::held(true, key, *down, owned, self.key_scan(key, *down, flags))
                } else {
                    // The client reported a modifier it never pressed as a
                    // key: press it around this key only, as Vibepollo does.
                    let mut held = HELD.lock().unwrap();
                    let count = held.get(&(true, key)).copied().unwrap_or(0);
                    let sent = if count == 0 {
                        Self::send(&self.with_modifiers(self.key_scan(key, true, flags), modifiers))
                    } else {
                        Ok(())
                    };
                    if sent.is_ok() {
                        held.insert((true, key), count + 1);
                    }
                    sent
                };
                // A press Windows refused is not held; a release is recorded
                // even when Windows refused it, or the key keeps repeating.
                if *down {
                    sent?;
                    self.keys.insert(key);
                    self.key_flags.insert(key, flags);
                    if !owned
                        && !is_modifier(key)
                        && let Some(delay) = self.policy.repeat_delay
                    {
                        self.repeat =
                            Some((key, flags, modifiers, std::time::Instant::now() + delay));
                    }
                } else {
                    self.keys.remove(&key);
                    self.key_flags.remove(&key);
                    if self.repeat.is_some_and(|(repeating, ..)| repeating == key) {
                        self.repeat = None;
                    }
                    sent?;
                }
            }
            Text(s) => {
                for v in s.encode_utf16() {
                    Self::send(&[Self::key(v, true, true), Self::key(v, false, true)])?;
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
            _ => {
                if self.gamepads.is_none() {
                    // Opening the driver enumerates devices: when it is missing,
                    // do not repeat that for every controller packet.
                    let now = std::time::Instant::now();
                    if self.gamepad_retry.is_some_and(|at| now < at) {
                        return Ok(());
                    }
                    match Gamepads::open_options(self.profile, self.policy.clone()) {
                        Ok(gamepads) => self.gamepads = Some(gamepads),
                        Err(error) => {
                            if self.gamepad_retry.is_none() {
                                tracing::warn!(
                                    error = format!("{error:#}"),
                                    "virtual gamepad driver unavailable; controller input is ignored"
                                );
                            }
                            self.gamepad_retry = Some(now + std::time::Duration::from_secs(10));
                            return Ok(());
                        }
                    }
                }
                self.gamepads.as_mut().unwrap().apply(e)?;
            }
        }
        Ok(())
    }
    /// Modifiers in a key-down packet that neither this client nor another
    /// holds as keys (Moonlight reports Shift/Ctrl/Alt both ways).
    fn synthetic_modifiers(&self, key: u32, reported: u8) -> u8 {
        if is_modifier(key) {
            return 0;
        }
        let held = HELD.lock().unwrap();
        let pressed = |keys: [u32; 3]| modifier_held(&self.keys, &held, keys);
        let mut synthetic = 0;
        for (mask, keys) in [
            (MODIFIER_SHIFT, [0x10, 0xa0, 0xa1]),
            (MODIFIER_CTRL, [0x11, 0xa2, 0xa3]),
            (MODIFIER_ALT, [0x12, 0xa4, 0xa5]),
        ] {
            if reported & mask != 0 && !pressed(keys) {
                synthetic |= mask;
            }
        }
        synthetic
    }
    /// The key's input surrounded by temporary presses of `modifiers`.
    fn with_modifiers(&self, key: INPUT, modifiers: u8) -> Vec<INPUT> {
        let generic = [
            (MODIFIER_SHIFT, 0x10),
            (MODIFIER_CTRL, 0x11),
            (MODIFIER_ALT, 0x12),
        ];
        let mut inputs = Vec::with_capacity(7);
        for (mask, vk) in generic {
            if modifiers & mask != 0 {
                inputs.push(self.key_scan(vk, true, 0));
            }
        }
        inputs.push(key);
        for (mask, vk) in generic.iter().rev() {
            if modifiers & mask != 0 {
                inputs.push(self.key_scan(*vk, false, 0));
            }
        }
        inputs
    }
    fn held(keyboard: bool, id: u32, down: bool, owned: bool, input: INPUT) -> Result<()> {
        Self::update_held(
            &mut HELD.lock().unwrap(),
            (keyboard, id),
            down,
            owned,
            input,
            Self::send,
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
/// Move the calling thread to the desktop that receives input, which is the
/// secure desktop while a UAC prompt or the lock screen shows. Only a host
/// running as SYSTEM may attach to it; elsewhere this fails harmlessly.
/// Returns whether the thread is now on the input desktop.
pub fn follow_input_desktop() -> bool {
    use windows::Win32::System::StationsAndDesktops::{
        CloseDesktop, DESKTOP_ACCESS_FLAGS, DF_ALLOWOTHERACCOUNTHOOK, OpenInputDesktop,
        SetThreadDesktop,
    };
    unsafe {
        let Ok(desktop) = OpenInputDesktop(
            DF_ALLOWOTHERACCOUNTHOOK,
            false,
            DESKTOP_ACCESS_FLAGS(windows::Win32::Foundation::GENERIC_ALL.0),
        ) else {
            return false;
        };
        let attached = SetThreadDesktop(desktop).is_ok();
        let _ = CloseDesktop(desktop);
        attached
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
impl Drop for Injector {
    fn drop(&mut self) {
        if self.left_release.take().is_some() {
            let _ = Self::held(false, 1, false, true, Self::button(1, false));
        }
        for key in &self.keys {
            let _ = Self::held(
                true,
                *key,
                false,
                true,
                self.key_scan(*key, false, self.key_flags.get(key).copied().unwrap_or(0)),
            );
        }
        for button in &self.buttons {
            let _ = Self::held(
                false,
                u32::from(*button),
                false,
                true,
                Self::button(*button, false),
            );
        }
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

#[cfg(test)]
#[path = "input/native_touch_tests.rs"]
mod native_touch_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn submit_touch(
        pointers: &mut BTreeMap<(u8, u32), u8>,
        profile: u16,
        global: u16,
        input: &Event,
    ) -> Option<Vec<u8>> {
        let update = gamepad_touch_request(pointers, profile, global, input)?;
        let packet = update.packet.clone();
        update.submitted(pointers);
        Some(packet)
    }

    #[test]
    fn failed_touch_submission_does_not_acquire_or_release_contact_slots() {
        let mut pointers = BTreeMap::new();
        drop(gamepad_touch_request(&pointers, 6, 2, &touch(2, 0, 1, 17)).unwrap());
        assert!(pointers.is_empty());
        submit_touch(&mut pointers, 6, 2, &touch(2, 0, 1, 17)).unwrap();
        for event in [2, 4, 6, 7] {
            drop(gamepad_touch_request(&pointers, 6, 2, &touch(2, 0, event, 17)).unwrap());
            assert_eq!(pointers, BTreeMap::from([((2, 17), 0)]));
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
