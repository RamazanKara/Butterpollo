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
    /// The last feedback report forwarded for each controller.
    last_feedback: BTreeMap<u16, (u16, Vec<u8>)>,
}
static SLOTS: std::sync::Mutex<[bool; 16]> = std::sync::Mutex::new([false; 16]);
static HELD: std::sync::Mutex<BTreeMap<(bool, u16), usize>> =
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
                self.ensure(u16::from(*id))?;
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
            Event::ControllerTouch {
                id,
                event,
                pointer,
                x,
                y,
                pressure,
            } => {
                self.ensure(u16::from(*id))?;
                if !matches!(self.profiles.get(&u16::from(*id)), Some(5 | 6)) {
                    return Ok(());
                }
                let mut b = request(22, Some(u32::from(self.active[&u16::from(*id)])));
                let event = match event {
                    0..=4 => *event,
                    6 => 4,
                    7 => 5,
                    _ => return Ok(()),
                };
                let key = (*id, *pointer);
                let slot = if event == 5 {
                    0
                } else if let Some(slot) = self.pointers.get(&key) {
                    *slot
                } else if matches!(event, 0 | 1) {
                    let Some(slot) = (0..2).find(|slot| {
                        !self
                            .pointers
                            .iter()
                            .any(|((pad, _), used)| pad == id && used == slot)
                    }) else {
                        return Ok(());
                    };
                    self.pointers.insert(key, slot);
                    slot
                } else {
                    return Ok(());
                };
                b.extend_from_slice(&[slot, event]);
                for f in [*x, *y, *pressure] {
                    b.extend_from_slice(&((f.clamp(0., 1.) * 65535.) as u16).to_le_bytes());
                }
                b.extend_from_slice(&[0, 0]);
                self.ioctl(0x805, &b, 0)?;
                if event == 5 {
                    self.pointers.retain(|(pad, _), _| pad != id);
                } else if matches!(event, 2 | 4) {
                    self.pointers.remove(&key);
                }
            }
            Event::Battery { id, state, percent } => {
                self.ensure(u16::from(*id))?;
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
    keys: BTreeSet<u16>,
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
    key_flags: BTreeMap<u16, u8>,
    /// Repeating key, its flags, the modifiers it adds and when it repeats.
    repeat: Option<(u16, u8, u8, std::time::Instant)>,
    scroll: [i32; 2],
    haptics: bool,
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
        let d = crate::capture::displays()?
            .into_iter()
            .find(|d| d.display_name == output || output.is_empty())
            .ok_or_else(|| anyhow::anyhow!("input display missing"))?;
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
            rect: RECT {
                left: d.x,
                top: d.y,
                right: d.x + d.width as i32,
                bottom: d.y + d.height as i32,
            },
        })
    }
    fn send(inputs: &[INPUT]) -> Result<()> {
        if unsafe { SendInput(inputs, size_of::<INPUT>() as i32) } != inputs.len() as u32 {
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
                    } else if matches!(key, 0x21..=0x2e | 0xa3 | 0xa5 | 0x5b | 0x5c | 0x5d | 0x6f) {
                        KEYEVENTF_EXTENDEDKEY
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    ..Default::default()
                },
            },
        }
    }
    fn key_scan(&self, key: u16, down: bool, flags: u8) -> INPUT {
        let mut input = Self::key(key, down, false);
        // Normalized Moonlight VKs always use the fixed US table. Non-normalized
        // keys follow the host's layout only when the administrator asks for it.
        let scan = if flags & 1 == 0 {
            crate::keylayout::SCANCODES[(key & 255) as usize] as u16
        } else if self.policy.always_send_scancodes && !matches!(key, 0x5b | 0x5c | 0x13) {
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
                    InjectSyntheticPointerInput(device, &contacts)?;
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
                self.inject_touches()?;
                self.touches.clear();
                return Ok(());
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
            self.inject_touches()?;
            if matches!(event, 2 | 4 | 6) {
                self.touches.remove(&id);
            }
            for p in self.touches.values_mut() {
                p.pointerInfo.pointerFlags &=
                    !(POINTER_FLAG_DOWN | POINTER_FLAG_UP | POINTER_FLAG_CANCELED);
                p.pointerInfo.pointerFlags |= POINTER_FLAG_UPDATE;
            }
            self.refreshed = std::time::Instant::now();
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
            InjectSyntheticPointerInput(
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
        if let Some(gamepads) = &mut self.gamepads {
            gamepads.refresh()?;
        }
        if let Some((key, flags, modifiers, due)) = self.repeat
            && std::time::Instant::now() >= due
        {
            Self::send(&self.with_modifiers(self.key_scan(key, true, flags), modifiers))?;
            self.repeat = Some((
                key,
                flags,
                modifiers,
                std::time::Instant::now() + self.policy.repeat_period,
            ));
        }
        if self.refreshed.elapsed() < std::time::Duration::from_millis(250) {
            return Ok(());
        }
        self.inject_touches()?;
        if self.pen.pointerInfo.pointerFlags != POINTER_FLAG_NONE
            && let Some(device) = self.pen_device
        {
            unsafe {
                InjectSyntheticPointerInput(
                    device,
                    &[POINTER_TYPE_INFO {
                        r#type: PT_PEN,
                        Anonymous: POINTER_TYPE_INFO_0 { penInfo: self.pen },
                    }],
                )?;
            }
        }
        self.refreshed = std::time::Instant::now();
        Ok(())
    }
    pub fn apply(&mut self, e: &Event) -> Result<()> {
        if !self.policy.allows(e) {
            return Ok(());
        }
        use Event::*;
        match e {
            Relative { x, y } => Self::send(&[Self::mouse(
                i32::from(*x),
                i32::from(*y),
                0,
                MOUSEEVENTF_MOVE,
            )])?,
            Absolute {
                x,
                y,
                width,
                height,
            } => unsafe {
                let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
                let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
                let w = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
                let h = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
                let px = self.rect.left
                    + (i64::from(*x).clamp(0, i64::from(*width))
                        * i64::from(self.rect.right - self.rect.left)
                        / i64::from(*width)) as i32;
                let py = self.rect.top
                    + (i64::from(*y).clamp(0, i64::from(*height))
                        * i64::from(self.rect.bottom - self.rect.top)
                        / i64::from(*height)) as i32;
                Self::send(&[Self::mouse(
                    ((i64::from(px - left) * 65535) / i64::from((w - 1).max(1))) as i32,
                    ((i64::from(py - top) * 65535) / i64::from((h - 1).max(1))) as i32,
                    0,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                )])?;
            },
            MouseButton { button, down } => {
                let owned = self.buttons.contains(button);
                Self::held(
                    false,
                    u16::from(*button),
                    *down,
                    owned,
                    Self::button(*button, *down),
                )?;
                if *down {
                    self.buttons.insert(*button);
                } else {
                    self.buttons.remove(button);
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
                let key = self.policy.key(*key);
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
                if modifiers == 0 {
                    Self::held(true, key, *down, owned, self.key_scan(key, *down, flags))?;
                } else {
                    // The client reported a modifier it never pressed as a
                    // key: press it around this key only, as Vibepollo does.
                    let mut held = HELD.lock().unwrap();
                    let count = held.get(&(true, key)).copied().unwrap_or(0);
                    if count == 0 {
                        Self::send(
                            &self.with_modifiers(self.key_scan(key, true, flags), modifiers),
                        )?;
                    }
                    held.insert((true, key), count + 1);
                }
                if *down {
                    self.keys.insert(key);
                    self.key_flags.insert(key, flags);
                    if !owned && !is_modifier(key) {
                        self.repeat = Some((
                            key,
                            flags,
                            modifiers,
                            std::time::Instant::now() + self.policy.repeat_delay,
                        ));
                    }
                } else {
                    self.keys.remove(&key);
                    self.key_flags.remove(&key);
                    if self.repeat.is_some_and(|(repeating, ..)| repeating == key) {
                        self.repeat = None;
                    }
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
                    self.gamepads =
                        Some(Gamepads::open_options(self.profile, self.policy.clone())?);
                }
                self.gamepads.as_mut().unwrap().apply(e)?;
            }
        }
        Ok(())
    }
    /// Modifiers in a key-down packet that neither this client nor another
    /// holds as keys (Moonlight reports Shift/Ctrl/Alt both ways).
    fn synthetic_modifiers(&self, key: u16, reported: u8) -> u8 {
        if is_modifier(key) {
            return 0;
        }
        let held = HELD.lock().unwrap();
        let pressed = |keys: [u16; 3]| {
            keys.iter()
                .any(|k| self.keys.contains(k) || held.contains_key(&(true, *k)))
        };
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
    fn held(keyboard: bool, id: u16, down: bool, owned: bool, input: INPUT) -> Result<()> {
        let mut held = HELD.lock().unwrap();
        let count = held.get(&(keyboard, id)).copied().unwrap_or(0);
        if down {
            if count == 0 || owned {
                Self::send(&[input])?;
            }
            if !owned {
                held.insert((keyboard, id), count + 1);
            }
        } else if owned {
            if count <= 1 {
                Self::send(&[input])?;
                held.remove(&(keyboard, id));
            } else {
                held.insert((keyboard, id), count - 1);
            }
        }
        Ok(())
    }
    pub fn feedback_allowed(&self, kind: u16) -> bool {
        !matches!(kind, 0x010b | 0x5500 | 0x5503) || (self.policy.forward_rumble && self.haptics)
    }
}
const MODIFIER_SHIFT: u8 = 0x01;
const MODIFIER_CTRL: u8 = 0x02;
const MODIFIER_ALT: u8 = 0x04;
fn is_modifier(key: u16) -> bool {
    matches!(key, 0x10..=0x12 | 0xa0..=0xa5 | 0x5b | 0x5c)
}
impl Drop for Injector {
    fn drop(&mut self) {
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
                u16::from(*button),
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
