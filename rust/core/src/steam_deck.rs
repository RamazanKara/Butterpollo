//! A virtual Steam Deck controller as the Deck itself presents it over USB
//! (28de:1205): its descriptors, its 64-byte input report and the feature
//! reports Steam and SDL send it. Served over USB/IP ([`crate::usbip`]), it
//! lets Steam on the host recognise a streaming Steam Deck as a Steam Deck,
//! with its trackpads, gyro and back grips.
//!
//! The report layout and button bits are SDL's `SteamDeckStatePacket_t` and
//! `SDL_hidapi_steamdeck.c`; the descriptors and the replies Steam expects
//! come from InputPlumber's Steam Deck target (GPL-3.0), which captured them
//! from a real Deck.
use crate::input::Input;
use crate::usbip::{Device, Setup, UsbDevice};

pub const VENDOR: u16 = 0x28de;
pub const PRODUCT: u16 = 0x1205;
/// The controller interface's interrupt IN endpoint.
pub const ENDPOINT: u8 = 3;
/// The real controller reports about every 4 ms whether or not anything
/// changed; SDL reads one within 16 ms of opening it to find the interface.
pub const REPORT_PERIOD: std::time::Duration = std::time::Duration::from_millis(4);

// ulButtonsL
const R2: u32 = 0x1;
const L2: u32 = 0x2;
const R: u32 = 0x4;
const L: u32 = 0x8;
const Y: u32 = 0x10;
const B: u32 = 0x20;
const X: u32 = 0x40;
const A: u32 = 0x80;
const DPAD_UP: u32 = 0x100;
const DPAD_RIGHT: u32 = 0x200;
const DPAD_LEFT: u32 = 0x400;
const DPAD_DOWN: u32 = 0x800;
const VIEW: u32 = 0x1000;
const STEAM: u32 = 0x2000;
const MENU: u32 = 0x4000;
const L5: u32 = 0x8000;
const R5: u32 = 0x1_0000;
const RIGHT_PAD_CLICK: u32 = 0x4_0000;
const PAD_TOUCH: [u32; 2] = [0x8_0000, 0x10_0000];
const L3: u32 = 0x40_0000;
const R3: u32 = 0x400_0000;
// ulButtonsH
const L4: u32 = 0x200;
const R4: u32 = 0x400;
const QAM: u32 = 0x4_0000;

/// Moonlight button flags and the Deck buttons they are, low and high word.
/// Moonlight's touchpad button is SDL's right trackpad click; its misc
/// button is the quick access menu (`...`). SDL gives Moonlight no left
/// trackpad click and no stick touch.
const BUTTONS: [(u32, u32, u32); 21] = [
    (0x1000, A, 0),
    (0x2000, B, 0),
    (0x4000, X, 0),
    (0x8000, Y, 0),
    (0x0001, DPAD_UP, 0),
    (0x0002, DPAD_DOWN, 0),
    (0x0004, DPAD_LEFT, 0),
    (0x0008, DPAD_RIGHT, 0),
    (0x0010, MENU, 0),
    (0x0020, VIEW, 0),
    (0x0040, L3, 0),
    (0x0080, R3, 0),
    (0x0100, L, 0),
    (0x0200, R, 0),
    (0x0400, STEAM, 0),
    (0x01_0000, 0, R4),
    (0x02_0000, 0, L4),
    (0x04_0000, R5, 0),
    (0x08_0000, L5, 0),
    (0x10_0000, RIGHT_PAD_CLICK, 0),
    (0x20_0000, 0, QAM),
];
/// The trigger travel past which the Deck also sets the digital L2 or R2
/// bit, as InputPlumber does (80%).
const TRIGGER_CLICK: u8 = 204;
const GRAVITY: f32 = 9.80665;

/// One trackpad: touching, where, and how hard.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Pad {
    pointer: Option<u32>,
    x: i16,
    y: i16,
    pressure: u16,
}

/// What the controller reports. Moonlight sends buttons, sticks and triggers
/// in one event, motion and touch in their own.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    buttons: u32,
    triggers: [u8; 2],
    /// Moonlight's, positive up like the Deck's own.
    sticks: [i16; 4],
    accel: [i16; 3],
    gyro: [i16; 3],
    pads: [Pad; 2],
}
impl State {
    /// Applies a Moonlight event for this controller. Returns whether the
    /// report changed; other kinds of event are ignored.
    pub fn apply(&mut self, event: &Input) -> bool {
        let before = self.clone();
        match *event {
            Input::Controller {
                buttons,
                left_trigger,
                right_trigger,
                sticks,
                ..
            } => {
                self.buttons = buttons;
                self.triggers = [left_trigger, right_trigger];
                self.sticks = sticks;
            }
            Input::Motion { kind, xyz, .. } => {
                // SDL reads the Deck's sensors in its own axes as
                // [X, Z, -Y]; Moonlight sends SDL's, accel in m/s² and gyro
                // in deg/s. Full scale is ±2 g and ±2000 deg/s.
                let scale = match kind {
                    1 => 32768. / (2. * GRAVITY),
                    2 => 32768. / 2000.,
                    _ => return false,
                };
                let raw = |v: f32| (v * scale).round().clamp(-32768., 32767.) as i16;
                let axes = [raw(xyz[0]), raw(-xyz[2]), raw(xyz[1])];
                if kind == 1 {
                    self.accel = axes;
                } else {
                    self.gyro = axes;
                }
            }
            Input::ControllerTouch {
                event,
                touchpad,
                pointer,
                x,
                y,
                pressure,
                ..
            } => {
                // SDL's touchpad 0 is the left trackpad and 1 the right.
                // Each takes one finger: a second one is ignored until the
                // first lifts.
                if event == 7 {
                    self.pads = Default::default();
                } else if let Some(pad) = self.pads.get_mut(usize::from(touchpad)) {
                    let mine = pad.pointer.is_none_or(|p| p == pointer);
                    match event {
                        1 | 3 if mine => {
                            pad.pointer = Some(pointer);
                            pad.x = pad_axis(x);
                            pad.y = pad_axis(1. - y);
                            pad.pressure = (pressure.clamp(0., 1.) * 32767.) as u16;
                        }
                        2 | 4 | 6 if pad.pointer == Some(pointer) => *pad = Pad::default(),
                        _ => {}
                    }
                }
            }
            _ => return false,
        }
        *self != before
    }
    /// The input report with packet number `packet`.
    pub fn report(&self, packet: u32) -> [u8; 64] {
        let mut low = 0;
        let mut high = 0;
        for (flag, l, h) in BUTTONS {
            if self.buttons & flag != 0 {
                low |= l;
                high |= h;
            }
        }
        if self.triggers[0] > TRIGGER_CLICK {
            low |= L2;
        }
        if self.triggers[1] > TRIGGER_CLICK {
            low |= R2;
        }
        for (pad, touch) in self.pads.iter().zip(PAD_TOUCH) {
            if pad.pointer.is_some() {
                low |= touch;
            }
        }
        let mut r = [0u8; 64];
        // ValveInReportHeader_t: version 1, ID_CONTROLLER_DECK_STATE, length.
        r[..4].copy_from_slice(&[1, 0, 9, 64]);
        r[4..8].copy_from_slice(&packet.to_le_bytes());
        r[8..12].copy_from_slice(&low.to_le_bytes());
        r[12..16].copy_from_slice(&high.to_le_bytes());
        let [left, right] = self.pads;
        // The quaternion (bytes 36..44) stays zero, as InputPlumber leaves
        // it; Steam reads the rates.
        let words = [
            (16, left.x),
            (18, left.y),
            (20, right.x),
            (22, right.y),
            (24, self.accel[0]),
            (26, self.accel[1]),
            (28, self.accel[2]),
            (30, self.gyro[0]),
            (32, self.gyro[1]),
            (34, self.gyro[2]),
            (48, self.sticks[0]),
            (50, self.sticks[1]),
            (52, self.sticks[2]),
            (54, self.sticks[3]),
        ];
        for (at, value) in words {
            r[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        // Raw triggers run 0..=32767; SDL maps them to its trigger axis.
        let unsigned = [
            (44, trigger(self.triggers[0])),
            (46, trigger(self.triggers[1])),
            (56, left.pressure),
            (58, right.pressure),
        ];
        for (at, value) in unsigned {
            r[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        r
    }
}
/// A trackpad position from 0..=1, left to right or bottom to top.
fn pad_axis(v: f32) -> i16 {
    ((v.clamp(0., 1.) - 0.5) * 65536.)
        .round()
        .clamp(-32768., 32767.) as i16
}
fn trigger(v: u8) -> u16 {
    (u32::from(v) * 32767 / 255) as u16
}

/// The virtual controller: its state, the feature report the next GET_REPORT
/// returns, and rumble Steam asked for.
pub struct SteamDeck {
    pub state: State,
    packet: u32,
    serial: String,
    /// The command byte of the last SET_REPORT, answered by the next GET_REPORT.
    command: u8,
    rumble: Option<(u16, u16)>,
}
impl SteamDeck {
    /// `serial` is what Steam keeps the controller's settings under; at most
    /// 20 characters are reported.
    pub fn new(serial: &str) -> Self {
        Self {
            state: State::default(),
            packet: 0,
            serial: serial.chars().take(20).collect(),
            command: 0,
            rumble: None,
        }
    }
    /// The motor speeds (low frequency, high frequency) Steam last set, once.
    pub fn take_rumble(&mut self) -> Option<(u16, u16)> {
        self.rumble.take()
    }
    fn set_report(&mut self, data: &[u8]) {
        let Some(&command) = data.first() else { return };
        self.command = command;
        // MsgSimpleRumbleCmd after the two header bytes: type, intensity,
        // left (low frequency) and right motor speed.
        if command == 0xeb && data.len() >= 9 {
            let speed = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]);
            self.rumble = Some((speed(5), speed(7)));
        }
    }
    fn feature_report(&self) -> [u8; 64] {
        let mut r = [0u8; 64];
        match self.command {
            // GetAttributesValues: the attributes a real Deck reports.
            0x83 => r[..ATTRIBUTES.len()].copy_from_slice(&ATTRIBUTES),
            // GetStringAttribute: the serial number.
            0xae => {
                r[..3].copy_from_slice(&[0xae, 0x14, 0x01]);
                r[3..3 + self.serial.len()].copy_from_slice(self.serial.as_bytes());
            }
            // GetChipId.
            0xba => {
                r[..3].copy_from_slice(&[0xba, 0x11, 0x00]);
                for (i, b) in r[3..18].iter_mut().enumerate() {
                    *b = i as u8;
                }
            }
            command => r[0] = command,
        }
        r
    }
}
const ATTRIBUTES: [u8; 42] = [
    0x83, 0x2d, 0x01, 0x05, 0x12, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x2b, 0x12, 0xa9,
    0x62, 0x04, 0xad, 0xf1, 0xe4, 0x65, 0x09, 0x2e, 0x00, 0x00, 0x00, 0x0b, 0xa0, 0x0f, 0x00, 0x00,
    0x0d, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x00,
];

impl Device for SteamDeck {
    fn describe(&self) -> UsbDevice {
        UsbDevice {
            vendor: VENDOR,
            product: PRODUCT,
            release: 0x0100,
            interfaces: INTERFACES
                .map(|(_, subclass, protocol, ..)| (3, subclass, protocol))
                .to_vec(),
        }
    }
    fn control(&mut self, setup: &Setup, data: &[u8]) -> Option<Vec<u8>> {
        let interface = usize::from(setup.index as u8);
        let reply = match (setup.request_type, setup.request) {
            // GET_STATUS for the device, an interface or an endpoint.
            (0x80..=0x82, 0x00) => vec![0, 0],
            // CLEAR_FEATURE, SET_FEATURE, SET_ADDRESS, SET_CONFIGURATION,
            // SET_INTERFACE.
            (0x00..=0x02, 0x01 | 0x03) | (0x00, 0x05 | 0x09) | (0x01, 0x0b) => vec![],
            (0x80, 0x08) => vec![1],
            (0x81, 0x0a) => vec![0],
            (0x80, 0x06) => match (setup.value >> 8, setup.value as u8) {
                (1, _) => device_descriptor().to_vec(),
                (2, _) => configuration_descriptor(),
                (3, 0) => vec![4, 3, 0x09, 0x04],
                (3, 1) => string_descriptor("Valve Software"),
                (3, 2) => string_descriptor("Steam Deck Controller"),
                // No other strings, no device qualifier (a full-speed
                // device), no BOS (USB 2.0).
                _ => return None,
            },
            (0x81, 0x06) => {
                let (_, _, _, _, report) = INTERFACES.get(interface)?;
                match setup.value >> 8 {
                    0x21 => hid_descriptor(interface).to_vec(),
                    0x22 => report.to_vec(),
                    _ => return None,
                }
            }
            // HID class: GET_REPORT, GET_IDLE, GET_PROTOCOL.
            (0xa1, 0x01) => match (setup.value >> 8, interface) {
                (1, 2) => self.state.report(self.packet).to_vec(),
                (3, 2) => self.feature_report().to_vec(),
                (1, _) => vec![0; 8],
                _ => return None,
            },
            (0xa1, 0x02) => vec![0],
            (0xa1, 0x03) => vec![1],
            // SET_REPORT, SET_IDLE, SET_PROTOCOL.
            (0x21, 0x09) => {
                if interface == 2 && setup.value >> 8 == 3 {
                    self.set_report(data);
                }
                vec![]
            }
            (0x21, 0x0a | 0x0b) => vec![],
            _ => return None,
        };
        Some(reply)
    }
    fn interrupt(&mut self, endpoint: u8) -> Option<Vec<u8>> {
        // The mouse and keyboard interfaces stay quiet, as on a Deck that
        // Steam has taken out of lizard mode.
        (endpoint == ENDPOINT).then(|| {
            self.packet = self.packet.wrapping_add(1);
            self.state.report(self.packet).to_vec()
        })
    }
}

fn device_descriptor() -> [u8; 18] {
    let [vl, vh] = VENDOR.to_le_bytes();
    let [pl, ph] = PRODUCT.to_le_bytes();
    // USB 2.0, class per interface, 64-byte EP0, release 1.00, strings
    // 1 and 2, no serial, one configuration.
    [
        18, 1, 0x00, 0x02, 0, 0, 0, 64, vl, vh, pl, ph, 0x00, 0x01, 1, 2, 0, 1,
    ]
}
/// Each interface: endpoint, subclass, protocol, HID country, report
/// descriptor. Interface 2 is the controller; Steam and SDL look for it.
const INTERFACES: [(u8, u8, u8, u8, &[u8]); 3] = [
    (0x81, 0, 2, 0, &MOUSE_REPORT),
    (0x82, 1, 1, 33, &KEYBOARD_REPORT),
    (0x83, 0, 0, 33, &CONTROLLER_REPORT),
];
fn hid_descriptor(interface: usize) -> [u8; 9] {
    let (_, _, _, country, report) = INTERFACES[interface];
    let [ll, lh] = (report.len() as u16).to_le_bytes();
    [9, 0x21, 0x11, 0x01, country, 1, 0x22, ll, lh]
}
fn configuration_descriptor() -> Vec<u8> {
    let mut d = vec![9, 2, 0, 0, INTERFACES.len() as u8, 1, 0, 0x80, 250];
    for (n, (endpoint, subclass, protocol, _, _)) in INTERFACES.iter().enumerate() {
        d.extend_from_slice(&[9, 4, n as u8, 0, 1, 3, *subclass, *protocol, 0]);
        d.extend_from_slice(&hid_descriptor(n));
        let size = if *endpoint == 0x80 | ENDPOINT { 64 } else { 8 };
        d.extend_from_slice(&[7, 5, *endpoint, 3, size, 0, 1]);
    }
    let total = (d.len() as u16).to_le_bytes();
    d[2..4].copy_from_slice(&total);
    d
}
fn string_descriptor(s: &str) -> Vec<u8> {
    let mut d = vec![0, 3];
    for unit in s.encode_utf16() {
        d.extend_from_slice(&unit.to_le_bytes());
    }
    d[0] = d.len() as u8;
    d
}

/// Boot-style mouse: two buttons, X, Y, wheel and pan.
const MOUSE_REPORT: [u8; 65] = [
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x02,
    0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02, 0x75, 0x06, 0x95, 0x01, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06,
    0x95, 0x01, 0x09, 0x38, 0x81, 0x06, 0x05, 0x0c, 0x0a, 0x38, 0x02, 0x95, 0x01, 0x81, 0x06, 0xc0,
    0xc0,
];
/// Boot keyboard: modifiers, a reserved byte and six keys.
const KEYBOARD_REPORT: [u8; 39] = [
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x81, 0x01, 0x19, 0x00, 0x29, 0x65, 0x15, 0x00, 0x25, 0x65,
    0x75, 0x08, 0x95, 0x06, 0x81, 0x00, 0xc0,
];
/// Vendor page 0xffff: a 64-byte input report and a 64-byte feature report.
const CONTROLLER_REPORT: [u8; 38] = [
    0x06, 0xff, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x09, 0x02, 0x09, 0x03, 0x15, 0x00, 0x26, 0xff, 0x00,
    0x75, 0x08, 0x95, 0x40, 0x81, 0x02, 0x09, 0x06, 0x09, 0x07, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75,
    0x08, 0x95, 0x40, 0xb1, 0x02, 0xc0,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn word(r: &[u8], at: usize) -> i16 {
        i16::from_le_bytes([r[at], r[at + 1]])
    }
    fn long(r: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(r[at..at + 4].try_into().unwrap())
    }
    fn setup(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> Setup {
        Setup {
            request_type,
            request,
            value,
            index,
            length,
        }
    }

    #[test]
    fn buttons_sticks_and_triggers_land_where_sdl_reads_them() {
        let mut state = State::default();
        let event = Input::Controller {
            id: 0,
            active: 1,
            // A, D-pad up, start, guide, R4, L5, touchpad, misc.
            buttons: 0x1000 | 0x1 | 0x10 | 0x400 | 0x01_0000 | 0x08_0000 | 0x10_0000 | 0x20_0000,
            left_trigger: 255,
            right_trigger: 100,
            sticks: [1000, -2000, 32767, -32768],
        };
        assert!(state.apply(&event));
        assert!(!state.apply(&event), "an unchanged state is no news");
        let r = state.report(7);
        assert_eq!(&r[..4], &[1, 0, 9, 64]);
        assert_eq!(long(&r, 4), 7);
        assert_eq!(
            long(&r, 8),
            A | DPAD_UP | MENU | STEAM | L5 | RIGHT_PAD_CLICK | L2
        );
        assert_eq!(long(&r, 12), R4 | QAM);
        assert_eq!(u16::from_le_bytes([r[44], r[45]]), 32767);
        assert_eq!(u16::from_le_bytes([r[46], r[47]]), 12849);
        // SDL negates the Deck's Y; Moonlight's Y is already up-positive.
        assert_eq!(
            [word(&r, 48), word(&r, 50), word(&r, 52), word(&r, 54)],
            [1000, -2000, 32767, -32768]
        );
    }

    #[test]
    fn motion_round_trips_through_sdls_decoding() {
        let mut state = State::default();
        state.apply(&Input::Motion {
            id: 0,
            kind: 2,
            xyz: [100., -50., 25.],
        });
        state.apply(&Input::Motion {
            id: 0,
            kind: 1,
            xyz: [0., 9.80665, -4.9],
        });
        let r = state.report(0);
        let (gx, gy, gz) = (word(&r, 30), word(&r, 32), word(&r, 34));
        // SDL: [X, Z, -Y] * 2000 / 32768 (in rad/s; deg/s here).
        let sdl = |v: i16| f32::from(v) * 2000. / 32768.;
        assert!((sdl(gx) - 100.).abs() < 0.1);
        assert!((sdl(gz) + 50.).abs() < 0.1);
        assert!((-sdl(gy) - 25.).abs() < 0.1);
        let (ax, ay, az) = (word(&r, 24), word(&r, 26), word(&r, 28));
        let g = |v: i16| f32::from(v) / 32768. * 2. * GRAVITY;
        assert!(g(ax).abs() < 0.01);
        assert!((g(az) - GRAVITY).abs() < 0.01);
        assert!((-g(ay) + 4.9).abs() < 0.01);
    }

    #[test]
    fn each_trackpad_follows_its_first_finger() {
        let touch = |touchpad, event, pointer, x, y| Input::ControllerTouch {
            id: 0,
            event,
            touchpad,
            pointer,
            x,
            y,
            pressure: 0.5,
        };
        let mut state = State::default();
        state.apply(&touch(1, 1, 9, 1.0, 0.0));
        // A second finger on the same pad is ignored.
        assert!(!state.apply(&touch(1, 1, 10, 0.5, 0.5)));
        state.apply(&touch(0, 1, 3, 0.25, 0.75));
        let r = state.report(0);
        assert_eq!(long(&r, 8), PAD_TOUCH[0] | PAD_TOUCH[1]);
        // SDL: x = X / 65536 + 0.5, y = -Y / 65536 + 0.5.
        assert_eq!((word(&r, 20), word(&r, 22)), (32767, 32767));
        assert_eq!((word(&r, 16), word(&r, 18)), (-16384, -16384));
        assert_eq!(u16::from_le_bytes([r[58], r[59]]), 16383);
        assert!(!state.apply(&touch(1, 2, 10, 0., 0.)), "not its finger");
        state.apply(&touch(1, 2, 9, 0., 0.));
        assert_eq!(long(&state.report(0), 8), PAD_TOUCH[0]);
        state.apply(&touch(0, 7, 0, 0., 0.));
        assert_eq!(state, State::default());
    }

    #[test]
    fn steam_finds_the_controller_interface_and_its_feature_replies() {
        let mut deck = SteamDeck::new("BUTTERPOLLO1");
        let device = deck.control(&setup(0x80, 6, 0x100, 0, 18), &[]).unwrap();
        assert_eq!(&device[8..12], &[0xde, 0x28, 0x05, 0x12]);
        let config = deck.control(&setup(0x80, 6, 0x200, 0, 255), &[]).unwrap();
        assert_eq!(config.len(), 9 + 3 * 25);
        assert_eq!(
            u16::from_le_bytes([config[2], config[3]]) as usize,
            config.len()
        );
        // The controller interface: HID, 64-byte interrupt IN on 0x83.
        assert_eq!(&config[9 + 50..9 + 50 + 9], &[9, 4, 2, 0, 1, 3, 0, 0, 0]);
        assert_eq!(&config[9 + 68..], &[7, 5, 0x83, 3, 64, 0, 1]);
        let report = deck.control(&setup(0x81, 6, 0x2200, 2, 38), &[]).unwrap();
        assert_eq!(report, CONTROLLER_REPORT);
        let product = deck
            .control(&setup(0x80, 6, 0x302, 0x409, 255), &[])
            .unwrap();
        assert_eq!(product[0] as usize, product.len());
        assert!(deck.control(&setup(0x80, 6, 0x600, 0, 10), &[]).is_none());

        // SET_REPORT then GET_REPORT, as Steam reads the serial number.
        let mut ask = [0u8; 64];
        ask[0] = 0xae;
        assert_eq!(
            deck.control(&setup(0x21, 9, 0x300, 2, 64), &ask),
            Some(vec![])
        );
        let serial = deck.control(&setup(0xa1, 1, 0x300, 2, 64), &[]).unwrap();
        assert_eq!(&serial[..15], b"\xae\x14\x01BUTTERPOLLO1");
        ask[0] = 0x83;
        deck.control(&setup(0x21, 9, 0x300, 2, 64), &ask);
        let attributes = deck.control(&setup(0xa1, 1, 0x300, 2, 64), &[]).unwrap();
        assert_eq!(&attributes[..4], &[0x83, 0x2d, 0x01, 0x05]);
        assert_eq!(deck.take_rumble(), None);
    }

    #[test]
    fn rumble_from_steam_is_kept_for_the_client() {
        let mut deck = SteamDeck::new("x");
        // ID_TRIGGER_RUMBLE_CMD as SDL sends it.
        let mut rumble = [0u8; 64];
        rumble[..11].copy_from_slice(&[0xeb, 9, 0, 0x02, 0x00, 0x34, 0x12, 0x78, 0x56, 2, 0]);
        deck.control(&setup(0x21, 9, 0x300, 2, 64), &rumble);
        assert_eq!(deck.take_rumble(), Some((0x1234, 0x5678)));
        assert_eq!(deck.take_rumble(), None);
    }

    #[test]
    fn only_the_controller_endpoint_reports() {
        let mut deck = SteamDeck::new("x");
        assert!(deck.interrupt(1).is_none());
        assert!(deck.interrupt(2).is_none());
        let first = deck.interrupt(3).unwrap();
        let second = deck.interrupt(3).unwrap();
        assert_eq!((long(&first, 4), long(&second, 4)), (1, 2));
    }
}
