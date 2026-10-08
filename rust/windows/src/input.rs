//! Input event dispatch and shared per-session injection state.

use anyhow::{Context, Result, bail};
use butterpollo_core::input::Input as Event;
use butterpollo_core::input_policy::{VHF_AUTO, gamepad_profile};
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

mod desktop;
mod gamepad_backend;
#[path = "input/gamepad_thread.rs"]
mod gamepad_thread;
mod gamepads;
mod keyboard_mouse;
mod steam_deck_pad;
mod touch_pen;

pub use desktop::{
    follow_input_desktop, input_desktop_name, keep_on_input_desktop, on_input_desktop,
    secure_desktop_shown,
};
pub use gamepad_thread::{GamepadThread, PadReport};
pub use gamepads::{Gamepads, open_interface};
use keyboard_mouse::{EXPLICIT_EXTENDED_KEY, inject, is_modifier, mapped_keyboard_identity};
use touch_pen::{inject_pointer, refresh_due};

static HELD: std::sync::Mutex<BTreeMap<(bool, u32), usize>> =
    std::sync::Mutex::new(BTreeMap::new());

pub fn capabilities(config: &butterpollo_core::config::Config) -> u32 {
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    let mut flags = 0;
    if config.boolean("mouse", true)
        && config.boolean("native_pen_touch", true)
        // SAFETY: The module and symbol names are terminated static strings; the returned address is only checked for presence.
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
            "vhf_xbox" | "vhf_xbox_one" | "vhf_switch"
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
    // SAFETY: display::mode returns a display DEVMODEW whose display position union field is initialized.
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
            // SAFETY: The injector still owns the pen device and the borrowed frame is initialized as pen input.
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
                // SAFETY: GetSystemMetrics uses valid metric constants and does not borrow any pointers.
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
    pub fn feedback_allowed(&self, kind: u16) -> bool {
        !matches!(kind, 0x010b | 0x5500 | 0x5503) || (self.policy.forward_rumble && self.haptics)
    }
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
        // SAFETY: These synthetic device handles are owned by the injector and taken so each is destroyed exactly once.
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
#[cfg(test)]
#[path = "input/native_touch_tests.rs"]
mod native_touch_tests;

#[cfg(test)]
mod tests {
    use super::*;

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
    #[test]
    fn rumble_feedback_obeys_forward_rumble_and_client_haptics() {
        let mut injector = recording();
        for forward in [false, true] {
            injector.policy.forward_rumble = forward;
            for haptics in [false, true] {
                injector.apply(&Event::Haptics(haptics)).unwrap();
                for kind in [0x010b, 0x5500, 0x5503] {
                    assert_eq!(injector.feedback_allowed(kind), forward && haptics);
                }
                assert!(injector.feedback_allowed(0x5502));
            }
        }
    }
    fn describe(input: &INPUT) -> String {
        // SAFETY: The test inputs are keyboard or mouse INPUTs with the union field matching their type initialized.
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
}
