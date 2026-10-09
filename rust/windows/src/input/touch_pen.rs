//! Synthetic touch contacts and pen frames with pointer refresh.

use super::*;

/// How often held touch contacts and an active pen are injected again:
/// Windows cancels pointer input that is not repeated.
const POINTER_REFRESH: std::time::Duration = std::time::Duration::from_millis(250);
/// Whether a pointer last injected at `last` is due again at `now`; if so,
/// its period restarts. Touch and pen keep their own periods: a pen that
/// keeps hovering must not hold back the repeat of a held touch, which
/// Windows would then cancel, nor the other way round.
pub(super) fn refresh_due(last: &mut std::time::Instant, now: std::time::Instant) -> bool {
    let due = now.saturating_duration_since(*last) >= POINTER_REFRESH;
    if due {
        *last = now;
    }
    due
}
impl Injector {
    fn location(&self, rect: RECT, x: f32, y: f32) -> POINT {
        let (x, y) = self.on_display(x, y);
        POINT {
            x: rect.left + ((rect.right - rect.left - 1).max(0) as f32 * x.clamp(0., 1.)) as i32,
            y: rect.top + ((rect.bottom - rect.top - 1).max(0) as f32 * y.clamp(0., 1.)) as i32,
        }
    }
    pub(super) fn inject_touches(&self) -> Result<()> {
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
                // SAFETY: The live touch device owns these contacts, whose union fields and pointer types are initialized as touch input.
                unsafe {
                    inject_pointer(device, &contacts)?;
                }
            }
        }
        Ok(())
    }
    /// Cancel every touch contact, as a client's cancel-all does.
    pub(super) fn cancel_touches(&mut self) -> Result<()> {
        for p in self.touches.values_mut() {
            pointer_event(&mut p.pointerInfo, 4, POINT::default());
        }
        let result = self.inject_touches();
        self.touches.clear();
        result
    }
    /// Lift the pen if it is active.
    pub(super) fn cancel_pen(&mut self) -> Result<()> {
        let (Some(device), Some(frame)) = (self.pen_device, pen_cancel(self.pen)) else {
            return Ok(());
        };
        // SAFETY: The pen device is still owned and frame is initialized as pen input before borrowing it for the call.
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
    pub(super) fn touch(
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
        // SAFETY: The synthetic device is retained by this injector, and the initialized touch frames remain live for injection.
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
    pub(super) fn pen(
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
            // SAFETY: The requested pen type and contact count are valid; the returned device is retained until injector teardown.
            None => *self.pen_device.insert(unsafe {
                CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_NONE)
                    .context("native pen input unavailable")?
            }),
        };
        // SAFETY: The live pen device and the initialized pen frame remain valid throughout injection.
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
}
/// Inject touch or pen input, retrying once on the input desktop.
pub(super) unsafe fn inject_pointer(
    device: HSYNTHETICPOINTERDEVICE,
    info: &[POINTER_TYPE_INFO],
) -> Result<()> {
    // SAFETY: The caller keeps the synthetic device alive and supplies initialized frames of its pointer type for both synchronous attempts.
    unsafe {
        match InjectSyntheticPointerInput(device, info) {
            Ok(()) => Ok(()),
            Err(_) if follow_input_desktop() => Ok(InjectSyntheticPointerInput(device, info)?),
            Err(error) => Err(error.into()),
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
mod tests {
    use super::*;
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
}
