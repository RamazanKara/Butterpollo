//! Display modes, refresh rates, and DPI scaling.
#![warn(clippy::undocumented_unsafe_blocks)]

use super::*;

const SCALES: [u32; 12] = [100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500];
#[repr(C)]
struct DpiGet {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    min: i32,
    current: i32,
    max: i32,
}
#[repr(C)]
struct DpiSet {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    relative: i32,
}
fn dpi_query(monitor: &Monitor) -> Result<DpiGet> {
    let mut value = DpiGet {
        header: header(
            DISPLAYCONFIG_DEVICE_INFO_TYPE(-3),
            size_of::<DpiGet>(),
            monitor.adapter,
            monitor.source,
        ),
        min: 0,
        current: 0,
        max: 0,
    };
    // SAFETY: DpiGet has the expected C layout and its header carries the matching type and size.
    unsafe {
        check(DisplayConfigGetDeviceInfo(&mut value.header))?;
    }
    Ok(value)
}
pub fn dpi_scale(monitor: &Monitor) -> Result<u32> {
    let value = dpi_query(monitor)?;
    let index = i64::from(value.current) - i64::from(value.min);
    SCALES
        .get(usize::try_from(index)?)
        .copied()
        .context("Windows reports an unknown DPI scale")
}
pub fn set_dpi_scale(monitor: &Monitor, percent: u32) -> Result<()> {
    let value = dpi_query(monitor)?;
    let desired = SCALES
        .iter()
        .position(|p| *p == percent)
        .context("unsupported Windows DPI scale")? as i64
        + i64::from(value.min);
    if desired < i64::from(value.min) || desired > i64::from(value.max) {
        bail!("requested DPI scale is outside this display's range");
    }
    if desired == i64::from(value.current) {
        return Ok(());
    }
    let set = DpiSet {
        header: header(
            DISPLAYCONFIG_DEVICE_INFO_TYPE(-4),
            size_of::<DpiSet>(),
            monitor.adapter,
            monitor.source,
        ),
        relative: desired as i32,
    };
    // SAFETY: DpiSet has the expected C layout and its header carries the matching type and size.
    unsafe { check(DisplayConfigSetDeviceInfo(&set.header)) }
}
pub fn virtual_scale(output: &str, configured: i64, width: u32, height: u32) -> Result<()> {
    if configured == 0 {
        return Ok(());
    }
    let monitors = monitors()?;
    let monitor = monitors
        .iter()
        .find(|m| m.display_name == output)
        .context("virtual display is unavailable for scaling")?;
    let desired = if configured < 0 {
        let ideal = f64::from(width.min(height)) * 100.0 / 864.0;
        *SCALES
            .iter()
            .min_by(|a, b| {
                (f64::from(**a) - ideal)
                    .abs()
                    .total_cmp(&(f64::from(**b) - ideal).abs())
            })
            .unwrap()
    } else {
        u32::try_from(configured)?
    };
    set_dpi_scale(monitor, desired)
}

pub fn mode(name: &str) -> Result<DEVMODEW> {
    // SAFETY: The display name is terminated and the writable DEVMODEW has its required size initialized.
    unsafe {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        if !EnumDisplaySettingsExW(
            PCWSTR(name.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut mode,
            ENUM_DISPLAY_SETTINGS_FLAGS(0),
        )
        .as_bool()
        {
            bail!("cannot read display mode");
        }
        Ok(mode)
    }
}
pub fn set_mode(name: &str, width: u32, height: u32, fps: u32) -> Result<()> {
    let mut next = mode(name)?;
    next.dmPelsWidth = width;
    next.dmPelsHeight = height;
    next.dmDisplayFrequency = fps;
    next.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
    apply_mode(name, &next)
}
fn apply_mode(name: &str, mode: &DEVMODEW) -> Result<()> {
    apply_mode_with(name, mode, CDS_FULLSCREEN)
}
pub(super) fn apply_mode_with(name: &str, mode: &DEVMODEW, flags: CDS_TYPE) -> Result<()> {
    // SAFETY: The display name is terminated and the borrowed display mode remains valid throughout the call.
    unsafe {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let result = ChangeDisplaySettingsExW(PCWSTR(name.as_ptr()), Some(mode), None, flags, None);
        if result != DISP_CHANGE_SUCCESSFUL {
            bail!("Windows refused the requested display mode ({})", result.0);
        }
        Ok(())
    }
}
/// Width, height and refresh in millihertz.
pub(super) type Timing = (u32, u32, u32);
/// Whether a display shows a mode: the same size, and a refresh within half a
/// hertz, as Windows reports rational rates such as 59.94 Hz.
pub(super) fn timing_matches(requested: Timing, actual: Timing) -> bool {
    (requested.0, requested.1) == (actual.0, actual.1) && requested.2.abs_diff(actual.2) <= 500
}
pub(super) fn format_timing((width, height, rate): Timing) -> String {
    format!("{width}x{height}@{:.3}", f64::from(rate) / 1000.)
}
/// The mode a display shows now.
pub(super) fn current_timing(output: &str) -> Result<Timing> {
    let topology = Topology::query()?;
    let monitor = topology
        .monitors()
        .into_iter()
        .find(|m| m.matches(output))
        .context("display unavailable")?;
    let current = mode(&monitor.display_name)?;
    Ok((
        current.dmPelsWidth,
        current.dmPelsHeight,
        topology.refresh(&monitor.device_id)?.0,
    ))
}
/// What became of a request for a virtual display's mode. Windows choosing
/// another mode is not an error: the stream continues and reports it.
#[derive(Debug, PartialEq)]
pub(super) enum ModeOutcome {
    Applied,
    /// Windows shows another mode, or refused the request.
    Kept(Timing),
    /// The display's mode could not be read back.
    Unknown,
}
pub(super) fn mode_outcome(requested: Timing, actual: Option<Timing>) -> ModeOutcome {
    match actual {
        Some(actual) if timing_matches(requested, actual) => ModeOutcome::Applied,
        Some(actual) => ModeOutcome::Kept(actual),
        None => ModeOutcome::Unknown,
    }
}
/// The refresh rates, in hertz, a display offers at a resolution.
pub(super) fn offered_rates(modes: &[(u32, u32, u32)], width: u32, height: u32) -> Vec<u32> {
    let mut rates: Vec<_> = modes
        .iter()
        .filter(|mode| (mode.0, mode.1) == (width, height))
        .map(|mode| mode.2)
        .collect();
    rates.sort_unstable();
    rates.dedup();
    rates
}
/// Every mode a display reports as (width, height, refresh Hz).
pub(super) fn supported_modes(output: &str) -> Vec<(u32, u32, u32)> {
    let name: Vec<_> = output.encode_utf16().chain(Some(0)).collect();
    let mut modes = Vec::new();
    for index in 0..4096 {
        let mut candidate = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        // SAFETY: The display name is terminated and the initialized DEVMODEW remains writable throughout enumeration.
        if !unsafe {
            EnumDisplaySettingsW(
                PCWSTR(name.as_ptr()),
                ENUM_DISPLAY_SETTINGS_MODE(index),
                &mut candidate,
            )
        }
        .as_bool()
        {
            break;
        }
        modes.push((
            candidate.dmPelsWidth,
            candidate.dmPelsHeight,
            candidate.dmDisplayFrequency,
        ));
    }
    modes
}
pub fn highest_refresh(
    output: &str,
    resolution: Option<(u32, u32)>,
) -> Result<butterpollo_core::framegen::Rate> {
    let monitors = monitors()?;
    let monitor = monitors
        .iter()
        .find(|m| m.matches(output))
        .or_else(|| {
            if output.is_empty() {
                monitors
                    .iter()
                    .find(|m| m.primary)
                    .or_else(|| monitors.first())
            } else {
                None
            }
        })
        .context("selected display unavailable")?;
    let current = mode(&monitor.display_name)?;
    let (width, height) = resolution.unwrap_or((current.dmPelsWidth, current.dmPelsHeight));
    let name: Vec<_> = monitor.display_name.encode_utf16().chain(Some(0)).collect();
    let mut best = 0;
    for index in 0..4096 {
        let mut candidate = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        // SAFETY: The display name is terminated and the initialized DEVMODEW remains writable throughout enumeration.
        if !unsafe {
            EnumDisplaySettingsW(
                PCWSTR(name.as_ptr()),
                ENUM_DISPLAY_SETTINGS_MODE(index),
                &mut candidate,
            )
        }
        .as_bool()
        {
            break;
        }
        if candidate.dmPelsWidth == width && candidate.dmPelsHeight == height {
            best = best.max(candidate.dmDisplayFrequency);
        }
    }
    if best == 0 {
        bail!("selected display has no mode for {width}x{height}");
    }
    Ok(butterpollo_core::framegen::Rate(best.saturating_mul(1000)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_virtual_display_mode_is_applied_only_at_its_size_and_rate() {
        let requested = (3840, 2160, 1_000_000);
        assert_eq!(
            mode_outcome(requested, Some((3840, 2160, 1_000_000))),
            ModeOutcome::Applied
        );
        assert_eq!(
            mode_outcome(requested, Some((3840, 2160, 999_600))),
            ModeOutcome::Applied
        );
        // The report: Windows recalled a 60 Hz mode for the display.
        assert_eq!(
            mode_outcome(requested, Some((3840, 2160, 60_000))),
            ModeOutcome::Kept((3840, 2160, 60_000))
        );
        assert_eq!(
            mode_outcome(requested, Some((2560, 1440, 1_000_000))),
            ModeOutcome::Kept((2560, 1440, 1_000_000))
        );
        assert_eq!(mode_outcome(requested, None), ModeOutcome::Unknown);
        assert!(timing_matches((1920, 1080, 60_000), (1920, 1080, 59_940)));
        assert!(!timing_matches(
            (1920, 1080, 116_000),
            (1920, 1080, 120_000)
        ));
        assert_eq!(format_timing((3840, 2160, 59_940)), "3840x2160@59.940");
    }
    #[test]
    fn offered_rates_list_each_refresh_of_the_requested_size_once() {
        let modes = [
            (3840, 2160, 60),
            (3840, 2160, 2000),
            (2560, 1440, 1000),
            (3840, 2160, 1000),
            (3840, 2160, 60),
        ];
        assert_eq!(offered_rates(&modes, 3840, 2160), [60, 1000, 2000]);
        assert!(offered_rates(&modes, 1920, 1080).is_empty());
    }
}
