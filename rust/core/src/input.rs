use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Relative {
        x: i16,
        y: i16,
    },
    Absolute {
        x: i16,
        y: i16,
        width: i16,
        height: i16,
    },
    MouseButton {
        button: u8,
        down: bool,
    },
    Scroll {
        amount: i16,
        horizontal: bool,
    },
    Keyboard {
        key: u16,
        modifiers: u8,
        flags: u8,
        down: bool,
    },
    Text(String),
    Controller {
        id: u16,
        active: u16,
        buttons: u32,
        left_trigger: u8,
        right_trigger: u8,
        sticks: [i16; 4],
    },
    Arrival {
        id: u8,
        kind: u8,
        capabilities: u16,
        buttons: u32,
    },
    Touch {
        event: u8,
        id: u32,
        x: f32,
        y: f32,
        pressure: f32,
        major: f32,
        minor: f32,
        rotation: u16,
    },
    Pen {
        event: u8,
        tool: u8,
        buttons: u8,
        x: f32,
        y: f32,
        pressure: f32,
        rotation: u16,
        tilt: u8,
    },
    ControllerTouch {
        id: u8,
        event: u8,
        touchpad: u8,
        pointer: u32,
        x: f32,
        y: f32,
        pressure: f32,
    },
    Motion {
        id: u8,
        kind: u8,
        xyz: [f32; 3],
    },
    Battery {
        id: u8,
        state: u8,
        percent: u8,
    },
    Haptics(bool),
}
fn be16(b: &[u8], n: usize) -> i16 {
    i16::from_be_bytes([b[n], b[n + 1]])
}
fn le16(b: &[u8], n: usize) -> u16 {
    u16::from_le_bytes([b[n], b[n + 1]])
}
fn le32(b: &[u8], n: usize) -> u32 {
    u32::from_le_bytes(b[n..n + 4].try_into().unwrap())
}
fn float(b: &[u8], n: usize) -> Result<f32> {
    let f = f32::from_le_bytes(b[n..n + 4].try_into().unwrap());
    if !f.is_finite() {
        bail!("non-finite input coordinate");
    }
    Ok(f)
}
pub fn decode(b: &[u8]) -> Result<Input> {
    if b.len() < 8 {
        bail!("short input header");
    }
    let declared = u32::from_be_bytes(b[..4].try_into().unwrap()) as usize;
    if declared != b.len() - 4 {
        bail!("input length does not match header");
    }
    let magic = le32(b, 4);
    let need = match magic {
        3 | 4 => 14,
        5 => 18,
        6 | 7 => 12,
        8 | 9 => 9,
        10 => 14,
        12 => 34,
        13 => 10,
        23 => 9,
        0x55000001 => 10,
        0x55000002 => 36,
        0x55000003 => 36,
        0x55000004 => 16,
        0x55000005 => 28,
        0x55000006 => 24,
        0x55000007 => 12,
        _ => bail!("unknown input magic {magic:#x}"),
    };
    if b.len() < need {
        bail!("short input packet");
    }
    if matches!(magic, 0x55000004..=0x55000007) && b[8] >= 16 {
        bail!("invalid controller number");
    }
    if matches!(magic, 0x55000002 | 0x55000003) && b[8] > 7 {
        bail!("invalid pointer event");
    }
    if magic == 0x55000003 && b[9] > 2 {
        bail!("invalid pen tool");
    }
    if magic == 0x55000005 && b[9] > 7 {
        bail!("invalid controller touch event");
    }
    if magic == 0x55000006 && !matches!(b[9], 1 | 2) {
        bail!("invalid motion sensor");
    }
    if magic == 0x55000007 && (b[9] > 5 || (b[10] > 100 && b[10] != 255)) {
        bail!("invalid battery report");
    }
    Ok(match magic {
        // Moonlight sets 0x8000 on every key code; only the low byte is the VK.
        3 | 4 => Input::Keyboard {
            key: le16(b, 9) & 0x00ff,
            modifiers: b[11],
            flags: b[8],
            down: magic == 3,
        },
        5 => {
            let (w, h) = (be16(b, 14), be16(b, 16));
            if w <= 0 || h <= 0 {
                bail!("invalid absolute mouse dimensions");
            }
            Input::Absolute {
                x: be16(b, 8),
                y: be16(b, 10),
                width: w,
                height: h,
            }
        }
        6 | 7 => Input::Relative {
            x: be16(b, 8),
            y: be16(b, 10),
        },
        8 | 9 => {
            if !(1..=5).contains(&b[8]) {
                bail!("invalid mouse button");
            }
            Input::MouseButton {
                button: b[8],
                down: magic == 8,
            }
        }
        10 => Input::Scroll {
            amount: be16(b, 8),
            horizontal: false,
        },
        12 => {
            let id = le16(b, 10);
            if id >= 16 {
                bail!("invalid controller number");
            }
            Input::Controller {
                id,
                active: le16(b, 12),
                buttons: u32::from(le16(b, 16)) | (u32::from(le16(b, 30)) << 16),
                left_trigger: b[18],
                right_trigger: b[19],
                sticks: [
                    le16(b, 20) as i16,
                    le16(b, 22) as i16,
                    le16(b, 24) as i16,
                    le16(b, 26) as i16,
                ],
            }
        }
        13 => Input::Haptics(le16(b, 8) != 0),
        23 => {
            if b.len() - 8 > 32 {
                bail!("text input too long");
            }
            Input::Text(
                std::str::from_utf8(&b[8..])?
                    .trim_end_matches('\0')
                    .to_owned(),
            )
        }
        0x55000001 => Input::Scroll {
            amount: be16(b, 8),
            horizontal: true,
        },
        0x55000002 => Input::Touch {
            event: b[8],
            id: le32(b, 12),
            x: float(b, 16)?,
            y: float(b, 20)?,
            pressure: float(b, 24)?,
            major: float(b, 28)?,
            minor: float(b, 32)?,
            rotation: le16(b, 10),
        },
        0x55000003 => Input::Pen {
            event: b[8],
            tool: b[9],
            buttons: b[10],
            x: float(b, 12)?,
            y: float(b, 16)?,
            pressure: float(b, 20)?,
            rotation: le16(b, 24),
            tilt: b[26],
        },
        0x55000004 => {
            if b[8] >= 16 {
                bail!("invalid controller number");
            }
            Input::Arrival {
                id: b[8],
                kind: b[9],
                capabilities: le16(b, 10),
                buttons: le32(b, 12),
            }
        }
        0x55000005 => Input::ControllerTouch {
            id: b[8],
            event: b[9],
            touchpad: b[11],
            pointer: le32(b, 12),
            x: float(b, 16)?,
            y: float(b, 20)?,
            pressure: float(b, 24)?,
        },
        0x55000006 => Input::Motion {
            id: b[8],
            kind: b[9],
            xyz: [float(b, 12)?, float(b, 16)?, float(b, 20)?],
        },
        0x55000007 => Input::Battery {
            id: b[8],
            state: b[9],
            percent: b[10],
        },
        _ => unreachable!(),
    })
}
#[derive(Debug, PartialEq, Eq)]
pub enum Batch {
    Merged,
    Skip,
    Stop,
}
impl Input {
    /// Only coalesce motion. Button, key and gesture transitions retain their order.
    pub fn merge(&mut self, newer: &Self) -> Batch {
        use Input::*;
        match (self, newer) {
            (Relative { x, y }, Relative { x: nx, y: ny }) => {
                match (x.checked_add(*nx), y.checked_add(*ny)) {
                    (Some(a), Some(b)) => {
                        *x = a;
                        *y = b;
                        Batch::Merged
                    }
                    _ => Batch::Stop,
                }
            }
            (
                Scroll { amount, horizontal },
                Scroll {
                    amount: n,
                    horizontal: h,
                },
            ) if horizontal == h => match amount.checked_add(*n) {
                Some(a) => {
                    *amount = a;
                    Batch::Merged
                }
                None => Batch::Stop,
            },
            (s @ Absolute { .. }, n @ Absolute { .. }) => {
                if let (
                    Absolute {
                        width: w,
                        height: h,
                        ..
                    },
                    Absolute {
                        width: nw,
                        height: nh,
                        ..
                    },
                ) = (&*s, n)
                    && (w != nw || h != nh)
                {
                    return Batch::Stop;
                }
                *s = n.clone();
                Batch::Merged
            }
            (s @ Controller { .. }, n @ Controller { .. }) => {
                if let (
                    Controller {
                        id,
                        active,
                        buttons,
                        ..
                    },
                    Controller {
                        id: ni,
                        active: na,
                        buttons: nb,
                        ..
                    },
                ) = (&*s, n)
                {
                    if active != na {
                        return Batch::Stop;
                    }
                    if id != ni {
                        return Batch::Skip;
                    }
                    if buttons != nb {
                        return Batch::Stop;
                    }
                }
                *s = n.clone();
                Batch::Merged
            }
            (s @ Touch { .. }, n @ Touch { .. }) => {
                if let (
                    Touch { event, id, .. },
                    Touch {
                        event: ne, id: ni, ..
                    },
                ) = (&*s, n)
                {
                    if !matches!(event, 0 | 3) || !matches!(ne, 0 | 3) {
                        return Batch::Stop;
                    }
                    if id != ni {
                        return Batch::Skip;
                    }
                    if event != ne {
                        return Batch::Stop;
                    }
                }
                *s = n.clone();
                Batch::Merged
            }
            (s @ Pen { .. }, n @ Pen { .. }) => {
                if let (
                    Pen {
                        event,
                        tool,
                        buttons,
                        ..
                    },
                    Pen {
                        event: ne,
                        tool: nt,
                        buttons: nb,
                        ..
                    },
                ) = (&*s, n)
                    && (!matches!(event, 0 | 3) || event != ne || tool != nt || buttons != nb)
                {
                    return Batch::Stop;
                }
                *s = n.clone();
                Batch::Merged
            }
            (s @ Motion { .. }, n @ Motion { .. }) => {
                if let (
                    Motion { id, kind, .. },
                    Motion {
                        id: ni, kind: nk, ..
                    },
                ) = (&*s, n)
                    && (id != ni || kind != nk)
                {
                    return Batch::Skip;
                }
                *s = n.clone();
                Batch::Merged
            }
            (s @ ControllerTouch { .. }, n @ ControllerTouch { .. }) => {
                if let (
                    ControllerTouch {
                        id,
                        event,
                        touchpad,
                        pointer,
                        ..
                    },
                    ControllerTouch {
                        id: ni,
                        event: ne,
                        touchpad: nt,
                        pointer: np,
                        ..
                    },
                ) = (&*s, n)
                {
                    if id != ni || touchpad != nt {
                        return Batch::Skip;
                    }
                    // Cancel-all affects every contact on this touchpad, even
                    // if its pointer ID differs from the move being batched.
                    if *event == 7 || *ne == 7 {
                        return Batch::Stop;
                    }
                    if pointer != np {
                        return Batch::Skip;
                    }
                    if !matches!(event, 0 | 3) || event != ne {
                        return Batch::Stop;
                    }
                }
                *s = n.clone();
                Batch::Merged
            }
            _ => Batch::Stop,
        }
    }
    pub fn required_permission(&self) -> u32 {
        match self {
            Self::Keyboard { .. } | Self::Text(_) => 1 << 12,
            Self::Touch { .. } => 1 << 9,
            Self::Pen { .. } => 1 << 10,
            Self::Relative { .. }
            | Self::Absolute { .. }
            | Self::MouseButton { .. }
            | Self::Scroll { .. } => 1 << 11,
            _ => 1 << 8,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overflow_leaves_both_axes_untouched() {
        let mut a = Input::Relative {
            x: 100,
            y: i16::MAX,
        };
        let b = Input::Relative { x: 1, y: 1 };
        assert_eq!(a.merge(&b), Batch::Stop);
        assert_eq!(
            a,
            Input::Relative {
                x: 100,
                y: i16::MAX
            }
        );
        assert_eq!(b, Input::Relative { x: 1, y: 1 });
    }
    #[test]
    fn relative_negative_and_scroll_merge() {
        let mut a = Input::Relative { x: -200, y: 50 };
        assert_eq!(a.merge(&Input::Relative { x: 5, y: -4 }), Batch::Merged);
        assert_eq!(a, Input::Relative { x: -195, y: 46 });
    }
    #[test]
    fn never_swallow_button_transition() {
        let mut a = Input::MouseButton {
            button: 1,
            down: true,
        };
        assert_eq!(
            a.merge(&Input::MouseButton {
                button: 1,
                down: false
            }),
            Batch::Stop
        );
    }
    #[test]
    fn malformed_packets_never_panic() {
        for len in 0..96 {
            for magic in [
                3u32,
                4,
                5,
                7,
                8,
                10,
                12,
                23,
                0x55000002,
                0x55000003,
                0x55000005,
                0x55000006,
                u32::MAX,
            ] {
                let mut b = vec![0; len];
                if len >= 8 {
                    b[..4].copy_from_slice(&((len - 4) as u32).to_be_bytes());
                    b[4..8].copy_from_slice(&magic.to_le_bytes());
                }
                let _ = decode(&b);
            }
        }
    }
    #[test]
    fn keyboard_codes_drop_moonlights_high_byte() {
        // Left arrow (VK 0x25) with Shift, as moonlight-qt and Artemis send it.
        let p = hex::decode("0000000a03000000002580010000").unwrap();
        assert_eq!(
            decode(&p).unwrap(),
            Input::Keyboard {
                key: 0x25,
                modifiers: 1,
                flags: 0,
                down: true
            }
        );
    }
    fn controller_touch(touchpad: u8, event: u8, pointer: u32, x: f32) -> Input {
        Input::ControllerTouch {
            id: 2,
            event,
            touchpad,
            pointer,
            x,
            y: 0.5,
            pressure: 0.75,
        }
    }
    #[test]
    fn controller_touch_decodes_the_wire_touchpad_index_without_changing_coordinates() {
        for touchpad in [0, 1, 2, 255] {
            let mut packet = vec![0; 28];
            packet[..4].copy_from_slice(&24u32.to_be_bytes());
            packet[4..8].copy_from_slice(&0x55000005u32.to_le_bytes());
            packet[8] = 2;
            packet[9] = 3;
            packet[11] = touchpad;
            packet[12..16].copy_from_slice(&17u32.to_le_bytes());
            packet[16..20].copy_from_slice(&0.25f32.to_le_bytes());
            packet[20..24].copy_from_slice(&0.5f32.to_le_bytes());
            packet[24..28].copy_from_slice(&0.75f32.to_le_bytes());
            assert_eq!(
                decode(&packet).unwrap(),
                controller_touch(touchpad, 3, 17, 0.25)
            );
        }
    }
    #[test]
    fn controller_touch_coalescing_keeps_surfaces_and_cancel_all_separate() {
        let mut primary = controller_touch(0, 3, 17, 0.25);
        let original = primary.clone();
        assert_eq!(primary.merge(&controller_touch(1, 3, 17, 0.9)), Batch::Skip);
        assert_eq!(primary, original);
        assert_eq!(primary.merge(&controller_touch(1, 7, 0, 0.0)), Batch::Skip);
        assert_eq!(primary.merge(&controller_touch(0, 7, 0, 0.0)), Batch::Stop);
        assert_eq!(primary, original);
        assert_eq!(primary.merge(&controller_touch(0, 2, 17, 0.0)), Batch::Stop);
        assert_eq!(
            primary.merge(&controller_touch(0, 3, 17, 0.5)),
            Batch::Merged
        );
        assert_eq!(primary, controller_touch(0, 3, 17, 0.5));
        let mut cancelled = controller_touch(0, 7, 0, 0.0);
        assert_eq!(cancelled.merge(&primary), Batch::Stop);
    }

    #[test]
    fn actual_moonlight_mouse_packet() {
        let p = hex::decode("0000000807000000fffb0004").unwrap();
        assert_eq!(decode(&p).unwrap(), Input::Relative { x: -5, y: 4 });
    }
}
