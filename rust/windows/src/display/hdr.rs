//! HDR requests and restoration on the currently connected display.
#![warn(clippy::undocumented_unsafe_blocks)]

use super::*;

pub fn set_hdr(m: &Monitor, enabled: bool) -> Result<()> {
    if enabled && !m.hdr_supported {
        bail!("selected display does not support HDR");
    }
    color_state::set(m.adapter, m.target, enabled)
}
/// What a stream's HDR request needs from a display. One without HDR is
/// left alone and the stream continues in SDR, as libdisplaydevice leaves
/// a display without an HDR state: the launch must not fail on it.
#[derive(Debug, PartialEq)]
pub(super) enum HdrAction {
    Keep,
    Skip,
    Set(bool),
}
pub(super) fn hdr_action(supported: bool, current: bool, requested: Option<bool>) -> HdrAction {
    match requested {
        Some(enabled) if enabled != current => {
            if enabled && !supported {
                HdrAction::Skip
            } else {
                HdrAction::Set(enabled)
            }
        }
        _ => HdrAction::Keep,
    }
}
/// Undo a stream's HDR change on the display as it is connected now, if it
/// still has the value the stream applied. Its adapter and target ids can
/// change while streaming (a driver reset, a TV that reconnects), so the
/// monitor recorded at stream start only names the device.
pub(super) fn restore_hdr(
    recorded: &Monitor,
    previous: bool,
    applied: bool,
    now: &[Monitor],
    set: impl FnOnce(&Monitor, bool) -> Result<()>,
) -> Result<()> {
    match now
        .iter()
        .find(|m| m.device_id == recorded.device_id && m.hdr_enabled == applied)
    {
        Some(monitor) => set(monitor, previous),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::monitor;
    use super::*;
    #[test]
    fn hdr_on_a_display_without_hdr_is_skipped_rather_than_failing_the_launch() {
        assert_eq!(hdr_action(false, false, Some(true)), HdrAction::Skip);
        assert_eq!(hdr_action(true, false, Some(true)), HdrAction::Set(true));
        assert_eq!(hdr_action(true, true, Some(false)), HdrAction::Set(false));
        assert_eq!(hdr_action(false, true, Some(false)), HdrAction::Set(false));
        assert_eq!(hdr_action(false, false, Some(false)), HdrAction::Keep);
        assert_eq!(hdr_action(true, true, Some(true)), HdrAction::Keep);
        assert_eq!(hdr_action(true, false, None), HdrAction::Keep);
    }
    #[test]
    fn hdr_is_restored_on_the_display_as_it_is_connected_now() {
        // Recorded at stream start; the adapter's LUID changed since.
        let recorded = monitor("tv", 1, false);
        let mut set = None;
        restore_hdr(
            &recorded,
            false,
            true,
            &[monitor("tv", 2, true)],
            |m, enabled| {
                set = Some((m.adapter.LowPart, enabled));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(set, Some((2, false)));
        // Changed back by the user, or gone: left alone.
        let untouched = |_: &Monitor, _| -> Result<()> { panic!("must not change HDR") };
        restore_hdr(
            &recorded,
            false,
            true,
            &[monitor("tv", 2, false)],
            untouched,
        )
        .unwrap();
        restore_hdr(&recorded, false, true, &[monitor("pc", 2, true)], untouched).unwrap();
        restore_hdr(&recorded, false, true, &[], untouched).unwrap();
    }
}
