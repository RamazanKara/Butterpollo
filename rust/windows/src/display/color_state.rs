//! Windows 11 separates HDR from wide-gamut SDR color management. The legacy
//! AdvancedColor bit can describe either and must not be reported as HDR.
//! SDK ABI: microsoft/win32metadata, RecompiledIdlHeaders/um/wingdi.h.
//! windows 0.62 lacks these newer structures; keep the small ABI definitions
//! here and fall back only when the operating system does not support them.
use super::*;

const GET_COLOR_INFO_2: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(15);
const SET_HDR_STATE: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(16);
const MODE_HDR: u32 = 2;

#[repr(C)]
#[derive(Default)]
struct ColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    flags: u32,
    encoding: u32,
    bits_per_channel: u32,
    active_mode: u32,
}

#[repr(C)]
struct SetHdr {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    enabled: u32,
}

#[derive(Default, Debug, PartialEq)]
pub(super) struct State {
    pub supported: bool,
    pub enabled: bool,
    modern: bool,
    requested: bool,
}

fn modern_state(flags: u32, mode: u32) -> State {
    State {
        supported: flags & (1 << 4) != 0 && flags & (1 << 3) == 0,
        // UserEnabled may be true while the mode is not actually active.
        enabled: mode == MODE_HDR,
        modern: true,
        requested: flags & (1 << 5) != 0,
    }
}

fn legacy_state(flags: u32) -> State {
    State {
        supported: flags & 1 != 0 && flags & (4 | 8) == 0,
        enabled: flags & 2 != 0 && flags & 4 == 0,
        modern: false,
        requested: flags & 2 != 0 && flags & 4 == 0,
    }
}

fn unsupported(code: i32) -> bool {
    matches!(code, 50 | 87 | 120) // NOT_SUPPORTED, INVALID_PARAMETER, CALL_NOT_IMPLEMENTED
}

pub(super) fn query(adapter: LUID, target: u32) -> Result<State> {
    let mut color = ColorInfo2 {
        header: header(GET_COLOR_INFO_2, size_of::<ColorInfo2>(), adapter, target),
        ..Default::default()
    };
    // SAFETY: ColorInfo2 has the C layout and header type/size required by GET_COLOR_INFO_2;
    // its initialized storage remains writable throughout the synchronous query.
    let code = unsafe { DisplayConfigGetDeviceInfo(&mut color.header) };
    if code == 0 {
        return Ok(modern_state(color.flags, color.active_mode));
    }
    if !unsupported(code) {
        check(code)?;
    }
    let mut legacy = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
        header: header(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(),
            adapter,
            target,
        ),
        ..Default::default()
    };
    // SAFETY: The initialized legacy structure has the matching header type and size
    // and remains writable throughout the query.
    check(unsafe { DisplayConfigGetDeviceInfo(&mut legacy.header) })?;
    // SAFETY: The successful query initialized the advanced-color flags union.
    Ok(legacy_state(unsafe { legacy.Anonymous.value }))
}

fn settled(state: &State, enabled: bool) -> bool {
    state.enabled == enabled && state.requested == enabled
}

fn apply(adapter: LUID, target: u32, enabled: bool, modern: bool) -> Result<()> {
    if modern {
        let state = SetHdr {
            header: header(SET_HDR_STATE, size_of::<SetHdr>(), adapter, target),
            enabled: u32::from(enabled),
        };
        // A failed HDR request must not fall back to toggling unrelated WCG.
        // SAFETY: SetHdr has the C layout and header type/size required by SET_HDR_STATE;
        // its initialized storage lives through the synchronous call.
        check(unsafe { DisplayConfigSetDeviceInfo(&state.header) })?;
    } else {
        let state = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
                size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>(),
                adapter,
                target,
            ),
            Anonymous: DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE_0 {
                value: u32::from(enabled),
            },
        };
        // SAFETY: The legacy state and flags are initialized with the matching header
        // type and size, and the structure lives through the synchronous call.
        check(unsafe { DisplayConfigSetDeviceInfo(&state.header) })?;
    }
    Ok(())
}

pub(super) fn set(adapter: LUID, target: u32, enabled: bool) -> Result<()> {
    let before = query(adapter, target)?;
    if settled(&before, enabled) {
        return Ok(());
    }
    apply(adapter, target, enabled, before.modern)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    settle(
        &before,
        enabled,
        || query(adapter, target),
        |previous| apply(adapter, target, previous, before.modern),
        || {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
            true
        },
    )
}

fn settle(
    before: &State,
    enabled: bool,
    mut observe: impl FnMut() -> Result<State>,
    mut restore: impl FnMut(bool) -> Result<()>,
    mut wait: impl FnMut() -> bool,
) -> Result<()> {
    // The successful SET owns this request until a query shows otherwise.
    let mut last_requested = enabled;
    loop {
        let detail = match observe() {
            Ok(current) => {
                last_requested = current.requested;
                if settled(&current, enabled) {
                    return Ok(());
                }
                "the active display mode did not change".to_string()
            }
            // Mode transitions can briefly make the display query fail.
            Err(error) => format!("the display state could not be read: {error:#}"),
        };
        if wait() {
            continue;
        }
        // Do not leave a delayed HDR request armed after failed setup.
        // Preserve a different user request observed during our wait.
        if last_requested == enabled && last_requested != before.requested {
            restore(before.requested).context(
                "HDR transition timed out and its previous request could not be restored",
            )?;
        }
        bail!("Windows accepted the HDR request but {detail}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_hdr_abi_matches_the_sdk() {
        assert_eq!(size_of::<ColorInfo2>(), 36);
        assert_eq!(std::mem::offset_of!(ColorInfo2, active_mode), 32);
        assert_eq!(size_of::<SetHdr>(), 24);
        assert_eq!(std::mem::offset_of!(SetHdr, enabled), 20);
    }

    #[test]
    fn wide_gamut_sdr_is_not_hdr_even_when_user_hdr_is_requested() {
        let flags = 1 | 2 | 16 | 32 | 64 | 128;
        assert_eq!(
            modern_state(flags, 1),
            State {
                supported: true,
                enabled: false,
                modern: true,
                requested: true
            }
        );
        assert!(modern_state(flags, MODE_HDR).enabled);
        assert!(!modern_state(flags, 0).enabled);
        assert!(!modern_state(1 | 2 | 64 | 128, 1).supported);
    }

    #[test]
    fn legacy_wide_gamut_and_policy_limits_do_not_enable_hdr() {
        assert_eq!(
            legacy_state(3),
            State {
                supported: true,
                enabled: true,
                modern: false,
                requested: true
            }
        );
        assert_eq!(
            legacy_state(7),
            State {
                supported: false,
                enabled: false,
                modern: false,
                requested: false
            }
        );
        assert!(!legacy_state(9).supported);
        assert!(!modern_state(1 | 8 | 16, 0).supported);
    }

    #[test]
    fn only_missing_color_api_uses_the_legacy_fallback() {
        for code in [50, 87, 120] {
            assert!(unsupported(code));
        }
        for code in [5, 31, 1167] {
            assert!(!unsupported(code));
        }
    }

    #[test]
    fn a_pending_hdr_enable_must_be_cancelled_on_restore() {
        let pending = modern_state(1 | 16 | 32, 1);
        assert!(!settled(&pending, false));
        assert!(!settled(&pending, true));
        assert!(settled(&modern_state(1 | 16, 1), false));
        assert!(settled(&modern_state(1 | 2 | 16 | 32, MODE_HDR), true));
    }

    #[test]
    fn hdr_transition_retries_temporary_query_failure() {
        let mut calls = 0;
        settle(
            &modern_state(17, 0),
            true,
            || {
                calls += 1;
                if calls == 1 {
                    bail!("display is transitioning");
                }
                Ok(modern_state(1 | 2 | 16 | 32, MODE_HDR))
            },
            |_| panic!("a successful transition must not roll back"),
            || true,
        )
        .unwrap();
        assert_eq!(calls, 2);
    }

    #[test]
    fn unreadable_hdr_transition_cancels_its_pending_request() {
        let mut restored = None;
        let result = settle(
            &modern_state(17, 0),
            true,
            || anyhow::bail!("display unavailable"),
            |previous| {
                restored = Some(previous);
                Ok(())
            },
            || false,
        );
        assert!(result.is_err());
        assert_eq!(restored, Some(false));
    }

    #[test]
    fn failed_hdr_transition_preserves_a_different_user_request() {
        let result = settle(
            &modern_state(17, 0),
            true,
            || Ok(modern_state(17, 0)),
            |_| panic!("must preserve the different user request"),
            || false,
        );
        assert!(result.is_err());
    }

    #[test]
    fn pending_hdr_timeout_restores_the_previous_request() {
        let mut restored = None;
        let result = settle(
            &modern_state(17, 0),
            true,
            || Ok(modern_state(17 | 32, 1)),
            |previous| {
                restored = Some(previous);
                Ok(())
            },
            || false,
        );
        assert!(result.is_err());
        assert_eq!(restored, Some(false));
    }

    #[test]
    fn failed_hdr_disable_restores_the_original_enabled_request() {
        let mut restored = None;
        let result = settle(
            &modern_state(17 | 2 | 32, MODE_HDR),
            false,
            || Ok(modern_state(17 | 2, MODE_HDR)),
            |previous| {
                restored = Some(previous);
                Ok(())
            },
            || false,
        );
        assert!(result.is_err());
        assert_eq!(restored, Some(true));
    }

    #[test]
    fn hdr_rollback_failure_remains_an_explicit_error() {
        let result = settle(
            &modern_state(17, 0),
            true,
            || Ok(modern_state(17 | 32, 1)),
            |_| anyhow::bail!("driver refused restoration"),
            || false,
        );
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("previous request could not be restored"));
        assert!(error.contains("driver refused restoration"));
    }
}
