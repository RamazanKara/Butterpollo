use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    mem::size_of,
    time::{Duration, Instant},
};
use windows::{
    Win32::{Devices::Display::*, Foundation::*, Graphics::Gdi::*, System::IO::DeviceIoControl},
    core::{GUID, PCWSTR},
};

mod color_state;
mod hotplug;
pub mod self_test;

fn check(code: i32) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(code).into())
    }
}
fn wide(b: &[u16]) -> String {
    String::from_utf16_lossy(&b[..b.iter().position(|v| *v == 0).unwrap_or(b.len())])
}
fn header(
    kind: DISPLAYCONFIG_DEVICE_INFO_TYPE,
    size: usize,
    luid: LUID,
    id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size as u32,
        adapterId: luid,
        id,
    }
}
#[derive(Clone, Serialize)]
pub struct Monitor {
    pub device_id: String,
    pub monitor_device_path: String,
    pub display_name: String,
    pub friendly_name: String,
    pub hdr_supported: bool,
    pub hdr_enabled: bool,
    pub primary: bool,
    #[serde(skip)]
    pub adapter: LUID,
    #[serde(skip)]
    pub target: u32,
    #[serde(skip)]
    pub source: u32,
}
impl Monitor {
    pub fn matches(&self, hint: &str) -> bool {
        [
            &self.device_id,
            &self.monitor_device_path,
            &self.display_name,
        ]
        .iter()
        .any(|value| value.eq_ignore_ascii_case(hint))
    }
}

fn monitor_edid(path: &str) -> Vec<u8> {
    let parts: Vec<_> = path.split('#').collect();
    if parts.len() < 3 || parts[1..3].iter().any(|p| p.contains(['\\', '/'])) {
        return Vec::new();
    }
    let key: Vec<u16> = format!(
        "SYSTEM\\CurrentControlSet\\Enum\\DISPLAY\\{}\\{}\\Device Parameters\0",
        parts[1], parts[2]
    )
    .encode_utf16()
    .collect();
    unsafe {
        use windows::Win32::System::Registry::*;
        let mut size = 0;
        if RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            windows::core::w!("EDID"),
            RRF_RT_REG_BINARY,
            None,
            None,
            Some(&mut size),
        )
        .is_err()
            || size > 65536
        {
            return Vec::new();
        }
        let mut bytes = vec![0; size as usize];
        if RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            windows::core::w!("EDID"),
            RRF_RT_REG_BINARY,
            None,
            Some(bytes.as_mut_ptr().cast()),
            Some(&mut size),
        )
        .is_err()
        {
            return Vec::new();
        }
        bytes.truncate(size as usize);
        bytes
    }
}
fn monitor_instance(path: &str) -> Result<String> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::*;
    let guid = GUID::from_u128(0xe6f07b5f_ee97_4a90_b076_33f57bf4eaa7);
    unsafe {
        let set = SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), None, DIGCF_DEVICEINTERFACE)?;
        let result = (|| -> Result<String> {
            let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
            let mut interface = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            SetupDiOpenDeviceInterfaceW(set, PCWSTR(path.as_ptr()), 0, Some(&mut interface))?;
            let mut required = 0;
            let mut device = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            let _ = SetupDiGetDeviceInterfaceDetailW(
                set,
                &interface,
                None,
                0,
                Some(&mut required),
                None,
            );
            if !(8..=65536).contains(&required) {
                bail!("invalid monitor interface size");
            }
            let mut buffer = vec![0u64; (required as usize).div_ceil(8)];
            let detail = buffer
                .as_mut_ptr()
                .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &interface,
                Some(detail),
                required,
                None,
                Some(&mut device),
            )?;
            let _ = SetupDiGetDeviceInstanceIdW(set, &device, None, Some(&mut required));
            if required == 0 || required > 32768 {
                bail!("invalid monitor instance size");
            }
            let mut instance = vec![0u16; required as usize];
            SetupDiGetDeviceInstanceIdW(set, &device, Some(&mut instance), None)?;
            Ok(wide(&instance))
        })();
        let _ = SetupDiDestroyDeviceInfoList(set);
        result
    }
}
fn monitor_id(path: &str) -> String {
    static IDS: std::sync::LazyLock<
        std::sync::Mutex<std::collections::BTreeMap<String, (Instant, String)>>,
    > = std::sync::LazyLock::new(Default::default);
    let mut ids = IDS.lock().unwrap();
    ids.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(1));
    if let Some((_, id)) = ids.get(path) {
        return id.clone();
    }
    let id = butterpollo_core::display_policy::legacy_device_id(
        path,
        monitor_instance(path).ok().as_deref(),
        &monitor_edid(path),
    );
    if ids.len() < 512 {
        ids.insert(path.into(), (Instant::now(), id.clone()));
    }
    id
}
pub struct Topology {
    paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}
fn extended_paths(
    candidates: &[Vec<DISPLAYCONFIG_PATH_INFO>],
) -> Result<Vec<DISPLAYCONFIG_PATH_INFO>> {
    type Source = (u32, i32, u32);
    fn assign(
        target: usize,
        candidates: &[Vec<DISPLAYCONFIG_PATH_INFO>],
        chosen: &mut [Option<DISPLAYCONFIG_PATH_INFO>],
        occupied: &mut std::collections::BTreeMap<Source, usize>,
        visited: &mut std::collections::BTreeSet<Source>,
    ) -> bool {
        for path in &candidates[target] {
            let source = (
                path.sourceInfo.adapterId.LowPart,
                path.sourceInfo.adapterId.HighPart,
                path.sourceInfo.id,
            );
            if !visited.insert(source) {
                continue;
            }
            if occupied
                .get(&source)
                .copied()
                .is_none_or(|previous| assign(previous, candidates, chosen, occupied, visited))
            {
                occupied.insert(source, target);
                chosen[target] = Some(*path);
                return true;
            }
        }
        false
    }
    let mut chosen = vec![None; candidates.len()];
    let mut occupied = std::collections::BTreeMap::new();
    for target in 0..candidates.len() {
        if !assign(
            target,
            candidates,
            &mut chosen,
            &mut occupied,
            &mut Default::default(),
        ) {
            bail!("connected displays cannot be assigned independent desktop sources");
        }
    }
    Ok(chosen.into_iter().flatten().collect())
}
/// An inactive route from QDC_ALL_PATHS made fit to switch on: Windows
/// chooses its modes, refresh and position. Such routes carry a zero
/// rotation and scaling and a stale refresh, which SetDisplayConfig refuses
/// with ERROR_INVALID_PARAMETER.
fn switch_on_route(path: &mut DISPLAYCONFIG_PATH_INFO) {
    path.flags = DISPLAYCONFIG_PATH_ACTIVE;
    path.sourceInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
    path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
    // A zero refresh lets Windows pick the display's best rate; it requires
    // unspecified scan-line ordering.
    path.targetInfo.refreshRate = DISPLAYCONFIG_RATIONAL::default();
    path.targetInfo.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED;
    if path.targetInfo.rotation.0 == 0 {
        path.targetInfo.rotation = DISPLAYCONFIG_ROTATION_IDENTITY;
    }
    if path.targetInfo.scaling.0 == 0 {
        path.targetInfo.scaling = DISPLAYCONFIG_SCALING_PREFERRED;
    }
}
/// Routes for an error report: source > target, flags, source/target mode
/// index, rotation, scaling, refresh and scan-line ordering.
fn describe_paths(paths: &[DISPLAYCONFIG_PATH_INFO]) -> String {
    paths
        .iter()
        .map(|p| unsafe {
            format!(
                "{}:{}>{}:{} f{} m{}/{} r{} s{} {}/{} o{}",
                p.sourceInfo.adapterId.LowPart,
                p.sourceInfo.id,
                p.targetInfo.adapterId.LowPart,
                p.targetInfo.id,
                p.flags,
                p.sourceInfo.Anonymous.modeInfoIdx as i32,
                p.targetInfo.Anonymous.modeInfoIdx as i32,
                p.targetInfo.rotation.0,
                p.targetInfo.scaling.0,
                p.targetInfo.refreshRate.Numerator,
                p.targetInfo.refreshRate.Denominator,
                p.targetInfo.scanLineOrdering.0,
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}
/// For each chosen route, whether it moves a display that is on to another
/// desktop source: the route is inactive but its target has an active one.
fn moving_targets(
    chosen: &[DISPLAYCONFIG_PATH_INFO],
    all: &[DISPLAYCONFIG_PATH_INFO],
) -> Vec<bool> {
    chosen
        .iter()
        .map(|path| {
            path.flags & DISPLAYCONFIG_PATH_ACTIVE == 0
                && all.iter().any(|other| {
                    other.flags & DISPLAYCONFIG_PATH_ACTIVE != 0
                        && other.targetInfo.adapterId == path.targetInfo.adapterId
                        && other.targetInfo.id == path.targetInfo.id
                })
        })
        .collect()
}
/// Chosen routes with the mode indices of those already active, so their
/// displays keep timing and position; newly switched-on routes get none.
fn keep_active_modes(paths: &[DISPLAYCONFIG_PATH_INFO]) -> Vec<DISPLAYCONFIG_PATH_INFO> {
    paths
        .iter()
        .map(|path| {
            let mut path = *path;
            if path.flags & DISPLAYCONFIG_PATH_ACTIVE == 0 {
                switch_on_route(&mut path);
            }
            path.flags = DISPLAYCONFIG_PATH_ACTIVE;
            path
        })
        .collect()
}
/// The active paths plus one route that switches `target` on, or None while
/// the target is not connected. Active paths keep their mode indices, so the
/// other displays keep their timings, positions and clone groups; Windows
/// chooses the new display's mode, refresh and position.
fn activation_paths(
    active: &[DISPLAYCONFIG_PATH_INFO],
    all: &[DISPLAYCONFIG_PATH_INFO],
    adapter: LUID,
    target: u32,
) -> Result<Option<Vec<DISPLAYCONFIG_PATH_INFO>>> {
    let ours = |p: &DISPLAYCONFIG_PATH_INFO| {
        p.targetInfo.adapterId == adapter && p.targetInfo.id == target
    };
    let source = |p: &DISPLAYCONFIG_PATH_INFO| {
        (
            p.sourceInfo.adapterId.LowPart,
            p.sourceInfo.adapterId.HighPart,
            p.sourceInfo.id,
        )
    };
    let routes: Vec<_> = all
        .iter()
        .filter(|p| ours(p) && p.targetInfo.targetAvailable.as_bool())
        .collect();
    if routes.is_empty() {
        return Ok(None);
    }
    let occupied: std::collections::BTreeSet<_> =
        active.iter().filter(|p| !ours(p)).map(source).collect();
    let mut route = **routes
        .iter()
        .find(|p| !occupied.contains(&source(p)))
        .context("no free desktop source for the virtual display")?;
    switch_on_route(&mut route);
    let mut paths: Vec<_> = active.iter().filter(|p| !ours(p)).copied().collect();
    paths.push(route);
    Ok(Some(paths))
}
/// Switch on a connected display beside the current desktop. Returns None
/// while the target is not connected, otherwise whether the other displays
/// kept their timings.
fn activate_target(adapter: LUID, target: u32) -> Result<Option<bool>> {
    let active = Topology::query()?;
    let all = Topology::query_all()?;
    let Some(paths) = activation_paths(&active.paths, &all.paths, adapter, target)? else {
        return Ok(None);
    };
    let flags = SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES;
    let kept = unsafe { SetDisplayConfig(Some(&paths), Some(&active.modes), flags) };
    if kept == 0 {
        return Ok(Some(true));
    }
    // Some drivers refuse supplied modes beside an unspecified one. Letting
    // Windows choose every mode can retime the other displays, which the
    // stream's layout restore undoes; a stream that cannot start cannot.
    let mut loose = paths;
    for path in &mut loose {
        path.sourceInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
        path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
    }
    check(unsafe { SetDisplayConfig(Some(&loose), None, flags) }).with_context(|| {
        format!(
            "Windows refused the display layout with the virtual display (supplied modes: {})",
            std::io::Error::from_raw_os_error(kept)
        )
    })?;
    Ok(Some(false))
}
impl Topology {
    fn rotations(&self) -> std::collections::BTreeMap<String, u32> {
        self.monitors()
            .iter()
            .filter_map(|m| {
                self.paths
                    .iter()
                    .find(|p| p.targetInfo.adapterId == m.adapter && p.targetInfo.id == m.target)
                    .map(|p| (m.device_id.clone(), p.targetInfo.rotation.0 as u32))
            })
            .collect()
    }
    fn set_rotations(&mut self, rotations: &std::collections::BTreeMap<String, u32>) -> Result<()> {
        let monitors = self.monitors();
        let mut changed = false;
        for monitor in monitors {
            if let Some(rotation) = rotations.get(&monitor.device_id) {
                if !(1..=4).contains(rotation) {
                    bail!("invalid saved display rotation");
                }
                if let Some(path) = self.paths.iter_mut().find(|p| {
                    p.targetInfo.adapterId == monitor.adapter && p.targetInfo.id == monitor.target
                }) && path.targetInfo.rotation.0 != *rotation as i32
                {
                    let swap = (path.targetInfo.rotation.0 - *rotation as i32).abs() % 2 != 0;
                    if swap
                        && let Some(mode) = self
                            .modes
                            .get_mut(unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize)
                        && mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE
                    {
                        unsafe {
                            std::mem::swap(
                                &mut mode.Anonymous.sourceMode.width,
                                &mut mode.Anonymous.sourceMode.height,
                            );
                        }
                    }
                    path.targetInfo.rotation = DISPLAYCONFIG_ROTATION(*rotation as i32);
                    path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
                    changed = true;
                }
            }
        }
        if changed {
            self.restore()?;
        }
        Ok(())
    }
    fn clone_groups(&self) -> Vec<Vec<String>> {
        let mut groups = std::collections::BTreeMap::<(u32, i32, u32), Vec<String>>::new();
        for monitor in self.monitors() {
            groups
                .entry((
                    monitor.adapter.LowPart,
                    monitor.adapter.HighPart,
                    monitor.source,
                ))
                .or_default()
                .push(monitor.device_id);
        }
        groups
            .into_values()
            .filter(|group| group.len() > 1)
            .collect()
    }
    fn restore_clone_groups(&mut self, groups: &[Vec<String>]) -> Result<()> {
        let monitors = self.monitors();
        let mut changed = false;
        let mut members = Vec::new();
        for group in groups {
            let selected: Vec<_> = group
                .iter()
                .filter_map(|id| monitors.iter().find(|m| m.device_id == *id))
                .collect();
            let Some(first) = selected.first() else {
                continue;
            };
            let source = self
                .paths
                .iter()
                .find(|p| {
                    p.targetInfo.adapterId == first.adapter && p.targetInfo.id == first.target
                })
                .context("clone source unavailable")?
                .sourceInfo;
            for monitor in &selected[1..] {
                if monitor.adapter != first.adapter {
                    bail!("clone targets now belong to different adapters");
                }
                let path = self
                    .paths
                    .iter_mut()
                    .find(|p| {
                        p.targetInfo.adapterId == monitor.adapter
                            && p.targetInfo.id == monitor.target
                    })
                    .context("clone target unavailable")?;
                path.sourceInfo = source;
                path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
                changed = true;
            }
            members.extend(selected.iter().map(|m| (m.adapter, m.target)));
        }
        if !changed {
            return Ok(());
        }
        let Err(exact) = self.restore() else {
            return Ok(());
        };
        // Windows can refuse to show the first member's desktop mode on the
        // others (ERROR_GEN_FAILURE). Let it choose a mode the whole group
        // can show; the caller sets every display's mode again afterwards.
        for path in &mut self.paths {
            if members.iter().any(|(adapter, target)| {
                path.targetInfo.adapterId == *adapter && path.targetInfo.id == *target
            }) {
                path.sourceInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
                path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
            }
        }
        if self.restore().is_ok() {
            return Ok(());
        }
        let mut loose = self.paths.clone();
        for path in &mut loose {
            path.sourceInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
            path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
        }
        let flags = SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES;
        check(unsafe { SetDisplayConfig(Some(&loose), None, flags) })
            .with_context(|| format!("Windows refused the cloned layout (saved modes: {exact:#})"))
    }
    pub fn nodes(&self) -> Result<Vec<butterpollo_core::topology::Node>> {
        use butterpollo_core::topology::{Kind, Mode, Node, Position};
        self.monitors()
            .into_iter()
            .map(|m| {
                let mode = mode(&m.display_name)?;
                let refresh = self.refresh(&m.device_id)?;
                let position = unsafe { mode.Anonymous1.Anonymous2.dmPosition };
                Ok(Node {
                    id: m.device_id.clone(),
                    label: m.friendly_name,
                    kind: Kind::Physical,
                    active: true,
                    primary: m.primary,
                    device_id: m.device_id,
                    desired_position: Position {
                        x: position.x,
                        y: position.y,
                    },
                    mode: Mode {
                        width: mode.dmPelsWidth,
                        height: mode.dmPelsHeight,
                        refresh_hz: f64::from(refresh.0) / 1000.0,
                    },
                })
            })
            .collect()
    }
    pub fn set_positions(
        &mut self,
        positions: &std::collections::BTreeMap<String, butterpollo_core::topology::Position>,
    ) -> Result<()> {
        let monitors = self.monitors();
        let mut moved = false;
        for monitor in monitors {
            let Some(position) = positions.get(&monitor.device_id) else {
                continue;
            };
            for path in &self.paths {
                if path.targetInfo.adapterId == monitor.adapter
                    && path.targetInfo.id == monitor.target
                {
                    let index = unsafe { path.sourceInfo.Anonymous.modeInfoIdx };
                    let source = self
                        .modes
                        .get_mut(index as usize)
                        .context("display source mode unavailable")?;
                    if source.infoType != DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
                        bail!("invalid display source mode");
                    }
                    let next = POINTL {
                        x: position.x,
                        y: position.y,
                    };
                    let current = unsafe { source.Anonymous.sourceMode.position };
                    if (current.x, current.y) != (next.x, next.y) {
                        source.Anonymous.sourceMode.position = next;
                        moved = true;
                    }
                }
            }
        }
        // Re-applying even an unchanged configuration lets Windows renegotiate
        // display timings: a TV at 1080p120 HDR came back at 60 Hz.
        if !moved {
            return Ok(());
        }
        self.restore()
    }
    pub fn query() -> Result<Self> {
        Self::query_with_flags(QDC_ONLY_ACTIVE_PATHS)
    }
    pub fn query_all() -> Result<Self> {
        Self::query_with_flags(QDC_ALL_PATHS)
    }
    fn query_with_flags(flags: QUERY_DISPLAY_CONFIG_FLAGS) -> Result<Self> {
        unsafe {
            for _ in 0..8 {
                let (mut np, mut nm) = (0, 0);
                GetDisplayConfigBufferSizes(flags, &mut np, &mut nm).ok()?;
                // QDC_ALL_PATHS includes every source/target combination, not
                // just attached monitors. VDDs can legitimately exceed 256.
                if np > 16384 || nm > 32768 {
                    bail!("display configuration exceeds the limit: {np} paths, {nm} modes");
                }
                let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
                let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
                let result = QueryDisplayConfig(
                    flags,
                    &mut np,
                    paths.as_mut_ptr(),
                    &mut nm,
                    modes.as_mut_ptr(),
                    None,
                );
                if result == ERROR_INSUFFICIENT_BUFFER {
                    continue;
                }
                result.ok()?;
                paths.truncate(np as usize);
                modes.truncate(nm as usize);
                return Ok(Self { paths, modes });
            }
            bail!("display topology is changing")
        }
    }
    pub fn restore(&self) -> Result<()> {
        unsafe {
            check(SetDisplayConfig(
                Some(&self.paths),
                Some(&self.modes),
                SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES,
            ))
        }
    }
    pub fn refresh(&self, id: &str) -> Result<butterpollo_core::framegen::Rate> {
        let monitor = self
            .monitors()
            .into_iter()
            .find(|m| m.device_id == id)
            .context("display unavailable")?;
        let path = self
            .paths
            .iter()
            .find(|p| {
                p.targetInfo.adapterId == monitor.adapter && p.targetInfo.id == monitor.target
            })
            .context("display path unavailable")?;
        let rate = path.targetInfo.refreshRate;
        if rate.Denominator == 0 {
            bail!("display refresh has no denominator");
        }
        Ok(butterpollo_core::framegen::Rate(u32::try_from(
            (u64::from(rate.Numerator) * 1000 + u64::from(rate.Denominator) / 2)
                / u64::from(rate.Denominator),
        )?))
    }
    pub fn set_mode_rate(
        name: &str,
        width: u32,
        height: u32,
        rate: butterpollo_core::framegen::Rate,
    ) -> Result<()> {
        if width == 0 || height == 0 || rate.0 == 0 {
            bail!("invalid display mode");
        }
        let mut topology = Self::query()?;
        let monitor = topology
            .monitors()
            .into_iter()
            .find(|m| m.matches(name))
            .context("display unavailable")?;
        // Windows chooses the target timing itself, even for the mode a display
        // already has, so a display already in this mode is left alone.
        let applied = || -> Result<bool> {
            let current = mode(&monitor.display_name)?;
            let actual = Self::query()?.refresh(&monitor.device_id)?;
            Ok(
                (current.dmPelsWidth, current.dmPelsHeight) == (width, height)
                    && actual.0.abs_diff(rate.0) <= 500,
            )
        };
        if applied()? {
            return Ok(());
        }
        let (num, den) = rate.rational();
        for path in &mut topology.paths {
            if path.targetInfo.adapterId == monitor.adapter && path.targetInfo.id == monitor.target
            {
                let index = unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize;
                let source = topology
                    .modes
                    .get_mut(index)
                    .context("display source mode unavailable")?;
                if source.infoType != DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
                    bail!("invalid source mode");
                }
                source.Anonymous.sourceMode.width = width;
                source.Anonymous.sourceMode.height = height;
                path.targetInfo.refreshRate = DISPLAYCONFIG_RATIONAL {
                    Numerator: num,
                    Denominator: den,
                };
                path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
            }
        }
        topology.restore()?;
        if applied()? {
            return Ok(());
        }
        // Windows reported success with another refresh rate: a TV whose native
        // timing is 4K60 turns 1080p120 into 60 Hz, and a 240 Hz monitor can
        // fall back to its 120 Hz default. The mode list applies the rate.
        let hz = rate.0.saturating_add(500) / 1000;
        tracing::debug!(output = %monitor.display_name, hz, "display rate applied through the mode list");
        let mut next = mode(&monitor.display_name)?;
        next.dmPelsWidth = width;
        next.dmPelsHeight = height;
        next.dmDisplayFrequency = hz;
        next.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
        apply_mode_with(&monitor.display_name, &next, CDS_TYPE(0))?;
        if !applied()? {
            let current = mode(&monitor.display_name)?;
            bail!(
                "display kept {}x{}@{} instead of {width}x{height}@{:.3}",
                current.dmPelsWidth,
                current.dmPelsHeight,
                current.dmDisplayFrequency,
                f64::from(rate.0) / 1000.
            );
        }
        Ok(())
    }
    pub fn set_active(ids: &[String]) -> Result<()> {
        Self::set_active_once(ids, true)
    }
    fn set_active_once(ids: &[String], may_move: bool) -> Result<()> {
        if ids.is_empty() {
            bail!("display topology cannot be empty");
        }
        let topology = Self::query_all()?;
        let mut candidates_by_target = Vec::new();
        for id in ids {
            let mut candidates = topology
                .paths
                .iter()
                .filter(|path| unsafe {
                    let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                        header: header(
                            DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                            size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(),
                            path.targetInfo.adapterId,
                            path.targetInfo.id,
                        ),
                        ..Default::default()
                    };
                    DisplayConfigGetDeviceInfo(&mut name.header) == 0
                        && (wide(&name.monitorDevicePath).eq_ignore_ascii_case(id)
                            || monitor_id(&wide(&name.monitorDevicePath)).eq_ignore_ascii_case(id))
                        && path.targetInfo.targetAvailable.as_bool()
                })
                .copied()
                .collect::<Vec<_>>();
            candidates.sort_by_key(|p| p.flags & DISPLAYCONFIG_PATH_ACTIVE == 0);
            if !candidates.is_empty() {
                candidates_by_target.push(candidates);
            }
        }
        let paths = extended_paths(&candidates_by_target)?;
        if paths.is_empty() {
            bail!("no saved display targets are currently connected");
        }
        let flags = SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES;
        // Displays that stay on their current source keep their timing and
        // position, such as a retained remote monitor that a restore must
        // leave alone; Windows chooses for the displays it switches on.
        // Windows can refuse that mix, so fall back to choosing every mode.
        let moving = moving_targets(&paths, &topology.paths);
        let mut paths = keep_active_modes(&paths);
        let kept = if paths.iter().any(|p| unsafe { p.sourceInfo.Anonymous.modeInfoIdx }
            != DISPLAYCONFIG_PATH_MODE_IDX_INVALID)
        {
            unsafe { SetDisplayConfig(Some(&paths), Some(&topology.modes), flags) }
        } else {
            -1
        };
        if kept == 0 {
            return Ok(());
        }
        // Windows refuses a display that changes desktop source while it is
        // on, such as one leaving a duplicate group (ERROR_INVALID_PARAMETER).
        // Switch it off with the others unchanged, then on at its new source.
        if may_move && moving.contains(&true) && moving.contains(&false) {
            let staying: Vec<_> = paths
                .iter()
                .zip(&moving)
                .filter(|(_, moving)| !**moving)
                .map(|(path, _)| *path)
                .collect();
            check(unsafe { SetDisplayConfig(Some(&staying), Some(&topology.modes), flags) })
                .context("cannot switch off the displays that change desktop source")?;
            return Self::set_active_once(ids, false);
        }
        let supplied = describe_paths(&paths);
        for path in &mut paths {
            path.sourceInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
            path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
        }
        unsafe { check(SetDisplayConfig(Some(&paths), None, flags)) }.with_context(|| {
            format!("Windows refused the display layout (with modes: {kept}; routes: {supplied})")
        })
    }
    pub fn monitors(&self) -> Vec<Monitor> {
        let primary = crate::capture::displays()
            .unwrap_or_default()
            .into_iter()
            .find(|d| d.primary)
            .map(|d| d.display_name);
        let mut paths: Vec<_> = self.paths.iter().collect();
        paths.sort_by_key(|p| p.flags & DISPLAYCONFIG_PATH_ACTIVE == 0);
        let mut targets = std::collections::BTreeSet::new();
        paths
            .into_iter()
            .filter(|p| {
                targets.insert((
                    p.targetInfo.adapterId.LowPart,
                    p.targetInfo.adapterId.HighPart,
                    p.targetInfo.id,
                ))
            })
            .filter_map(|p| unsafe {
                let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                    header: header(
                        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                        size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
                        p.sourceInfo.adapterId,
                        p.sourceInfo.id,
                    ),
                    ..Default::default()
                };
                let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                    header: header(
                        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                        size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(),
                        p.targetInfo.adapterId,
                        p.targetInfo.id,
                    ),
                    ..Default::default()
                };
                if DisplayConfigGetDeviceInfo(&mut source.header) != 0
                    || DisplayConfigGetDeviceInfo(&mut target.header) != 0
                {
                    return None;
                }
                let color =
                    color_state::query(p.targetInfo.adapterId, p.targetInfo.id).unwrap_or_default();
                let display_name = wide(&source.viewGdiDeviceName);
                let monitor_device_path = wide(&target.monitorDevicePath);
                Some(Monitor {
                    device_id: monitor_id(&monitor_device_path),
                    monitor_device_path,
                    friendly_name: wide(&target.monitorFriendlyDeviceName),
                    primary: primary.as_ref() == Some(&display_name),
                    display_name,
                    hdr_supported: color.supported,
                    hdr_enabled: color.enabled,
                    adapter: p.targetInfo.adapterId,
                    target: p.targetInfo.id,
                    source: p.sourceInfo.id,
                })
            })
            .collect()
    }
    /// Whether a path still leads to this monitor, as `monitors` would list
    /// it: the lease heartbeat's check each second, without the DXGI and
    /// colour queries that take nearly all of the half millisecond.
    fn shows(&self, monitor: &Monitor) -> bool {
        self.paths.iter().any(|p| unsafe {
            if p.targetInfo.adapterId != monitor.adapter || p.targetInfo.id != monitor.target {
                return false;
            }
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: header(
                    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
                    p.sourceInfo.adapterId,
                    p.sourceInfo.id,
                ),
                ..Default::default()
            };
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: header(
                    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(),
                    p.targetInfo.adapterId,
                    p.targetInfo.id,
                ),
                ..Default::default()
            };
            DisplayConfigGetDeviceInfo(&mut source.header) == 0
                && DisplayConfigGetDeviceInfo(&mut target.header) == 0
                && wide(&target.monitorDevicePath)
                    .eq_ignore_ascii_case(&monitor.monitor_device_path)
        })
    }
}
pub fn monitors() -> Result<Vec<Monitor>> {
    Ok(Topology::query()?.monitors())
}
pub fn edid_refresh(hint: &str, targets: &[u32]) -> Result<serde_json::Value> {
    let monitors = Topology::query_all()?.monitors();
    let monitor = monitors
        .iter()
        .find(|monitor| monitor.matches(hint) || monitor.friendly_name.eq_ignore_ascii_case(hint))
        .context("display device not found for EDID refresh validation")?;
    let parts: Vec<_> = monitor.monitor_device_path.split('#').collect();
    let mut bytes = Vec::new();
    if parts.len() >= 3 && parts[1..3].iter().all(|part| !part.contains(['\\', '/'])) {
        let key = format!(
            "SYSTEM\\CurrentControlSet\\Enum\\DISPLAY\\{}\\{}\\Device Parameters",
            parts[1], parts[2]
        );
        let key: Vec<_> = key.encode_utf16().chain(Some(0)).collect();
        let name: Vec<_> = "EDID\0".encode_utf16().collect();
        let mut size = 0u32;
        unsafe {
            use windows::Win32::System::Registry::*;
            if RegGetValueW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(key.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_BINARY,
                None,
                None,
                Some(&mut size),
            )
            .is_ok()
                && size <= 65536
            {
                bytes.resize(size as usize, 0);
                if RegGetValueW(
                    HKEY_LOCAL_MACHINE,
                    PCWSTR(key.as_ptr()),
                    PCWSTR(name.as_ptr()),
                    RRF_RT_REG_BINARY,
                    None,
                    Some(bytes.as_mut_ptr().cast()),
                    Some(&mut size),
                )
                .is_err()
                {
                    bytes.clear();
                }
            }
        }
    }
    let info = butterpollo_core::edid::Refresh::parse(&bytes);
    let mut response = serde_json::to_value(&info)?;
    response["status"] = true.into();
    response["device_id"] = monitor.device_id.clone().into();
    response["device_label"] = monitor.friendly_name.clone().into();
    response["targets"] = targets
        .iter()
        .map(|hz| {
            let (supported, method) = info.support(*hz);
            serde_json::json!({"hz":hz,"supported":supported,"method":method})
        })
        .collect::<Vec<_>>()
        .into();
    Ok(response)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub nodes: Vec<butterpollo_core::topology::Node>,
    pub hdr: std::collections::BTreeMap<String, bool>,
    #[serde(default)]
    pub clone_groups: Vec<Vec<String>>,
    #[serde(default)]
    pub scale: std::collections::BTreeMap<String, u32>,
    #[serde(default)]
    pub rotation: std::collections::BTreeMap<String, u32>,
}
impl Snapshot {
    pub fn read(path: &std::path::Path) -> Result<Self> {
        if std::fs::metadata(path)?.len() > 4 * 1024 * 1024 {
            bail!("display snapshot exceeds its limit");
        }
        let value = serde_json::from_slice(&std::fs::read(path)?)?;
        let mut snapshot = Self::decode(&value)?;
        let available = Topology::query_all()?.monitors();
        let normalized = |id: &str| {
            available
                .iter()
                .find(|m| m.matches(id))
                .map_or_else(|| id.to_owned(), |m| m.device_id.clone())
        };
        for node in &mut snapshot.nodes {
            node.id = normalized(&node.id);
            node.device_id = normalized(&node.device_id);
        }
        snapshot.hdr = snapshot
            .hdr
            .into_iter()
            .map(|(id, v)| (normalized(&id), v))
            .collect();
        snapshot.scale = snapshot
            .scale
            .into_iter()
            .map(|(id, v)| (normalized(&id), v))
            .collect();
        snapshot.rotation = snapshot
            .rotation
            .into_iter()
            .map(|(id, v)| (normalized(&id), v))
            .collect();
        snapshot.clone_groups = snapshot
            .clone_groups
            .iter()
            .map(|group| group.iter().map(|id| normalized(id)).collect())
            .collect();
        Ok(snapshot)
    }
    pub fn decode(value: &serde_json::Value) -> Result<Self> {
        if value.get("version").is_some() {
            let snapshot: Self = serde_json::from_value(value.clone())?;
            if snapshot.version != 1
                || snapshot.nodes.is_empty()
                || snapshot.nodes.len() > 64
                || snapshot.nodes.iter().any(|node| {
                    node.mode.width == 0
                        || node.mode.height == 0
                        || !node.mode.refresh_hz.is_finite()
                        || !(1.0..=1000.0).contains(&node.mode.refresh_hz)
                })
            {
                bail!("invalid Rust display snapshot");
            }
            return Ok(snapshot);
        }
        use butterpollo_core::topology::{Kind, Mode, Node, Position};
        let groups: Vec<Vec<String>> = serde_json::from_value(
            value
                .get("topology")
                .cloned()
                .context("saved display topology is missing")?,
        )?;
        let modes = value["modes"]
            .as_object()
            .context("saved display modes are missing")?;
        if groups.len() > 64 || modes.len() > 64 {
            bail!("saved display count exceeds its limit");
        }
        let active: std::collections::BTreeSet<_> = groups.iter().flatten().collect();
        let mut snapshot = Self {
            version: 1,
            nodes: vec![],
            hdr: Default::default(),
            clone_groups: groups.iter().filter(|g| g.len() > 1).cloned().collect(),
            scale: Default::default(),
            rotation: Default::default(),
        };
        for (id, mode) in modes {
            let number = |key| mode[key].as_u64().context("invalid saved display mode");
            let (width, height, num, den) = (
                u32::try_from(number("w")?)?,
                u32::try_from(number("h")?)?,
                number("num")?,
                number("den")?,
            );
            if width == 0 || height == 0 || width > 16384 || height > 16384 || den == 0 || num == 0
            {
                bail!("invalid saved display mode");
            }
            let x = i32::try_from(value["origins"][id]["x"].as_i64().unwrap_or(0))?;
            let y = i32::try_from(value["origins"][id]["y"].as_i64().unwrap_or(0))?;
            snapshot.nodes.push(Node {
                id: id.clone(),
                device_id: id.clone(),
                label: id.clone(),
                kind: Kind::Physical,
                active: active.contains(id),
                primary: value["primary"].as_str() == Some(id),
                desired_position: Position { x, y },
                mode: Mode {
                    width,
                    height,
                    refresh_hz: num as f64 / den as f64,
                },
            });
            if let Some(enabled) = match value["hdr"][id].as_str() {
                Some("on") => Some(true),
                Some("off") => Some(false),
                _ => value["hdr"][id].as_bool(),
            } {
                snapshot.hdr.insert(id.clone(), enabled);
            }
            if let Some(degrees) = value["layouts"][id]["rotation"].as_u64()
                && matches!(degrees, 0 | 90 | 180 | 270)
            {
                snapshot
                    .rotation
                    .insert(id.clone(), degrees as u32 / 90 + 1);
            }
        }
        if snapshot.nodes.is_empty() {
            bail!("saved display snapshot is empty");
        }
        Ok(snapshot)
    }
    pub fn capture() -> Result<Self> {
        let topology = Topology::query()?;
        Ok(Self {
            version: 1,
            nodes: topology.nodes()?,
            hdr: topology
                .monitors()
                .into_iter()
                .map(|m| (m.device_id, m.hdr_enabled))
                .collect(),
            rotation: topology.rotations(),
            clone_groups: topology.clone_groups(),
            scale: topology
                .monitors()
                .iter()
                .filter_map(|m| {
                    dpi_scale(m)
                        .ok()
                        .map(|percent| (m.device_id.clone(), percent))
                })
                .collect(),
        })
    }
    pub fn restore(&self) -> Result<()> {
        self.restore_excluding(&[])
    }
    /// Whether every display this layout turns on is connected now.
    pub fn displays_connected(&self) -> bool {
        let Ok(available) = Topology::query_all().map(|t| t.monitors()) else {
            return false;
        };
        let mut active = self.nodes.iter().filter(|n| n.active).peekable();
        active.peek().is_some()
            && active.all(|n| available.iter().any(|m| m.device_id == n.device_id))
    }
    pub fn restore_excluding(&self, excluded: &[String]) -> Result<()> {
        if self.version != 1 {
            bail!("unsupported Rust display snapshot version");
        }
        let current = Topology::query()?;
        let available = Topology::query_all()?.monitors();
        let preserve = |id: &str| excluded.iter().any(|e| id.eq_ignore_ascii_case(e));
        let mut ids = self
            .nodes
            .iter()
            .filter(|n| {
                n.active
                    && !preserve(&n.device_id)
                    && available.iter().any(|m| m.device_id == n.device_id)
            })
            .map(|n| n.device_id.clone())
            .collect::<Vec<_>>();
        ids.extend(
            current
                .monitors()
                .iter()
                .filter(|m| preserve(&m.device_id))
                .map(|m| m.device_id.clone()),
        );
        Topology::set_active(&ids).context("cannot activate the saved displays")?;
        // A display that cannot take back its old setting must not leave the
        // rest of the layout unrestored; report every such failure at the end.
        let mut failures = Vec::new();
        if let Err(error) = Topology::query().and_then(|mut t| {
            t.set_rotations(
                &self
                    .rotation
                    .iter()
                    .filter(|(id, _)| !preserve(id))
                    .map(|(id, r)| (id.clone(), *r))
                    .collect(),
            )
        }) {
            failures.push(format!("rotation: {error:#}"));
        }
        let monitors = monitors()?;
        for n in &self.nodes {
            if preserve(&n.device_id) {
                continue;
            }
            let Some(m) = monitors.iter().find(|m| m.device_id == n.device_id) else {
                continue;
            };
            let mut step = |what: &str, result: Result<()>| {
                if let Err(error) = result {
                    failures.push(format!(
                        "{} ({}) {what}: {error:#}",
                        n.label, m.display_name
                    ));
                }
            };
            step(
                &format!(
                    "mode {}x{}@{:.3}",
                    n.mode.width, n.mode.height, n.mode.refresh_hz
                ),
                Topology::set_mode_rate(
                    &m.display_name,
                    n.mode.width,
                    n.mode.height,
                    butterpollo_core::framegen::Rate((n.mode.refresh_hz * 1000.0).round() as u32),
                ),
            );
            if let Some(enabled) = self.hdr.get(&n.device_id) {
                step(&format!("HDR {enabled}"), set_hdr(m, *enabled));
            }
            if let Some(percent) = self.scale.get(&n.device_id) {
                step(&format!("scale {percent}%"), set_dpi_scale(m, *percent));
            }
        }
        // set_active gave every display its own desktop source. Join clone
        // groups first: placing their members separately would put two
        // desktops at the same origin.
        let mut topology = Topology::query()?;
        topology
            .restore_clone_groups(
                &self
                    .clone_groups
                    .iter()
                    .filter(|group| group.iter().all(|id| !preserve(id)))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
            .context("cannot restore the cloned displays")?;
        topology = Topology::query()?;
        topology
            .set_positions(
                &self
                    .nodes
                    .iter()
                    .filter(|n| !preserve(&n.device_id))
                    .map(|n| (n.device_id.clone(), n.desired_position))
                    .collect(),
            )
            .context("cannot restore the display positions")?;
        // Moving displays can renegotiate a display's timing (a TV at 120 Hz
        // came back at 60 Hz), so check every rate again. A clone member's
        // GDI name changed when it joined its group; find it by device.
        for n in self.nodes.iter().filter(|n| !preserve(&n.device_id)) {
            let Some(m) = monitors.iter().find(|m| m.device_id == n.device_id) else {
                continue;
            };
            if failures
                .iter()
                .any(|f| f.contains(&format!("({})", m.display_name)))
            {
                continue;
            }
            if let Err(error) = Topology::set_mode_rate(
                &n.device_id,
                n.mode.width,
                n.mode.height,
                butterpollo_core::framegen::Rate((n.mode.refresh_hz * 1000.0).round() as u32),
            ) {
                failures.push(format!(
                    "{} ({}) mode after layout: {error:#}",
                    n.label, m.display_name
                ));
            }
        }
        if !failures.is_empty() {
            bail!("display layout restored except {}", failures.join("; "));
        }
        Ok(())
    }
}

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

type PositionChanges = std::collections::BTreeMap<
    String,
    (
        butterpollo_core::topology::Position,
        butterpollo_core::topology::Position,
    ),
>;
static POSITIONS: std::sync::Mutex<PositionChanges> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());
pub fn baseline_nodes() -> Result<Vec<butterpollo_core::topology::Node>> {
    let mut nodes = Topology::query()?.nodes()?;
    let changes = POSITIONS.lock().unwrap();
    for node in &mut nodes {
        if let Some((before, _)) = changes.get(&node.device_id) {
            node.desired_position = *before;
        }
    }
    Ok(nodes)
}
pub fn apply_layout(nodes: &[butterpollo_core::topology::Node]) -> Result<()> {
    use butterpollo_core::topology::Kind;
    let mut topology = Topology::query()?;
    let current = topology.nodes()?;
    let monitors = topology.monitors();
    let mut changes = POSITIONS.lock().unwrap();
    for node in nodes.iter().filter(|n| n.kind == Kind::Physical) {
        if let Some(old) = current.iter().find(|old| old.device_id == node.device_id) {
            let before = changes
                .get(&node.device_id)
                .map_or(old.desired_position, |(before, _)| *before);
            if before != node.desired_position {
                let output = &monitors
                    .iter()
                    .find(|m| m.device_id == node.device_id)
                    .context("display disappeared")?
                    .display_name;
                crate::display_recovery::position(
                    &node.device_id,
                    output,
                    before,
                    node.desired_position,
                )?;
                changes.insert(node.device_id.clone(), (before, node.desired_position));
            }
        }
    }
    topology.set_positions(
        &nodes
            .iter()
            .map(|n| (n.device_id.clone(), n.desired_position))
            .collect(),
    )
}
pub fn restore_positions() -> Result<()> {
    let mut changes = POSITIONS.lock().unwrap();
    if changes.is_empty() {
        return Ok(());
    }
    let mut topology = Topology::query()?;
    let nodes = topology.nodes()?;
    let positions: std::collections::BTreeMap<_, _> = changes
        .iter()
        .filter_map(|(id, (before, applied))| {
            nodes
                .iter()
                .find(|n| &n.device_id == id && n.desired_position == *applied)
                .map(|_| (id.clone(), *before))
        })
        .collect();
    if !positions.is_empty() {
        topology.set_positions(&positions)?;
    }
    for id in changes.keys() {
        crate::display_recovery::release_position(id)?;
    }
    changes.clear();
    Ok(())
}
pub fn set_hdr(m: &Monitor, enabled: bool) -> Result<()> {
    if enabled && !m.hdr_supported {
        bail!("selected display does not support HDR");
    }
    color_state::set(m.adapter, m.target, enabled)
}
pub fn mode(name: &str) -> Result<DEVMODEW> {
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
fn apply_mode_with(name: &str, mode: &DEVMODEW, flags: CDS_TYPE) -> Result<()> {
    unsafe {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let result = ChangeDisplaySettingsExW(PCWSTR(name.as_ptr()), Some(mode), None, flags, None);
        if result != DISP_CHANGE_SUCCESSFUL {
            bail!("Windows refused the requested display mode ({})", result.0);
        }
        Ok(())
    }
}
const NAMESPACE: [u8; 16] = [
    0x84, 0x42, 0x86, 0xa2, 0xfe, 0x77, 0x36, 0x43, 0xa8, 0x28, 0, 0xfe, 0xec, 0x89, 0xeb, 0xac,
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DriverProtocol {
    Legacy35,
    Secure36,
}
impl DriverProtocol {
    fn parse(version: &[u8]) -> Result<Self> {
        if version.len() != 24 || version[..16] != NAMESPACE {
            bail!("invalid virtual display protocol response");
        }
        let major = u16::from_le_bytes(version[16..18].try_into().unwrap());
        let minor = u16::from_le_bytes(version[18..20].try_into().unwrap());
        if major != 3 || minor < 5 {
            bail!("unsupported virtual display protocol {major}.{minor}; requires 3.5+");
        }
        Ok(if minor == 5 {
            Self::Legacy35
        } else {
            Self::Secure36
        })
    }
    fn create_function(self) -> u32 {
        match self {
            Self::Legacy35 => 0x901,
            Self::Secure36 => 0x90c,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Legacy35 => "3.5",
            Self::Secure36 => "3.6+",
        }
    }
}
struct Driver {
    handle: HANDLE,
    protocol: DriverProtocol,
}
impl Driver {
    fn open() -> Result<Self> {
        let mut driver = Self {
            handle: crate::input::open_interface(GUID::from_u128(
                0x5f894d6c_3a69_48a2_86ef_e4c671932d63,
            ))?,
            protocol: DriverProtocol::Legacy35,
        };
        let version = driver.ioctl(0x900, 0, &[], 24)?;
        driver.protocol = DriverProtocol::parse(&version)?;
        Ok(driver)
    }
    fn ioctl(&self, function: u32, access: u32, input: &[u8], size: usize) -> Result<Vec<u8>> {
        unsafe {
            let mut output = vec![0; size];
            let mut bytes = 0;
            DeviceIoControl(
                self.handle,
                (0x22 << 16) | (access << 14) | (function << 2),
                if input.is_empty() {
                    None
                } else {
                    Some(input.as_ptr().cast())
                },
                input.len() as u32,
                if size == 0 {
                    None
                } else {
                    Some(output.as_mut_ptr().cast())
                },
                size as u32,
                Some(&mut bytes),
                None,
            )?;
            if bytes as usize > size {
                bail!("invalid virtual display response length");
            }
            output.truncate(bytes as usize);
            Ok(output)
        }
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}
/// Windows 11 (build 22000) or later. Vibepollo keeps hosts on Windows 10 on
/// the physical display unless configured otherwise: virtual-display capture
/// depends on Windows 11 capture features.
pub fn windows_11() -> bool {
    static BUILD: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    BUILD
        .get_or_init(|| unsafe {
            use windows::Win32::System::Registry::*;
            let mut text = [0u16; 32];
            let mut size = (text.len() * 2) as u32;
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                windows::core::w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
                windows::core::w!("CurrentBuildNumber"),
                RRF_RT_REG_SZ,
                None,
                Some(text.as_mut_ptr().cast()),
                Some(&mut size),
            )
            .ok()
            .ok()?;
            wide(&text).trim().parse().ok()
        })
        .is_none_or(|build| build >= 22000)
}
pub fn virtual_display_available() -> bool {
    Driver::open().is_ok()
}
pub fn virtual_display_status() -> serde_json::Value {
    match Driver::open() {
        Ok(driver) => {
            serde_json::json!({"capable":true,"ready":true,"reason":"","protocol":driver.protocol.name()})
        }
        Err(error) => {
            serde_json::json!({"capable":false,"ready":false,"reason":format!("{error:#}"),"protocol":"3.5+"})
        }
    }
}
fn display_label(name: &str) -> [u8; 32] {
    let mut label = [0u8; 32];
    let name: Vec<_> = name
        .bytes()
        .filter(|c| (0x20..=0x7e).contains(c))
        .take(31)
        .collect();
    let end = name.iter().rposition(|c| *c != b' ').map_or(0, |n| n + 1);
    if end == 0 {
        label[..b"Butterpollo".len()].copy_from_slice(b"Butterpollo");
    } else {
        label[..end].copy_from_slice(&name[..end]);
    }
    label
}
#[derive(Clone, Debug)]
pub struct VirtualOptions {
    pub label: String,
    pub peak_nits: u32,
}
impl Default for VirtualOptions {
    fn default() -> Self {
        Self {
            label: "Butterpollo".into(),
            peak_nits: 1000,
        }
    }
}
fn temporary_request(
    lease: u64,
    id: u64,
    mode: (u32, u32, u32),
    options: &VirtualOptions,
    protocol: DriverProtocol,
    capability: &[u8; 32],
) -> Vec<u8> {
    let mut request = NAMESPACE.to_vec();
    request.extend_from_slice(&lease.to_le_bytes());
    request.extend_from_slice(&id.to_le_bytes());
    for value in [mode.0, mode.1, 600, 340, mode.2, 10000] {
        request.extend_from_slice(&value.to_le_bytes());
    }
    request.extend_from_slice(&display_label(&options.label));
    request.extend_from_slice(&0u32.to_le_bytes()); // Retain Windows identity across sessions.
    if protocol == DriverProtocol::Secure36 {
        request.extend_from_slice(&options.peak_nits.clamp(400, 2000).to_le_bytes());
        request.extend_from_slice(capability);
    } else {
        request.extend_from_slice(&0u32.to_le_bytes()); // Legacy reserved field.
    }
    request
}
fn permanent_request(count: u32) -> Result<Vec<u8>> {
    if count > 4 {
        bail!("permanent virtual display count must be between 0 and 4");
    }
    let mut request = NAMESPACE.to_vec();
    for value in [count, 0, 1920, 1080, 600, 340, 60000] {
        request.extend_from_slice(&value.to_le_bytes());
    }
    request.extend_from_slice(&display_label("Butterpollo"));
    Ok(request)
}
fn permanent_response(bytes: &[u8]) -> Result<u32> {
    if bytes.len() != 80 || bytes[..16] != NAMESPACE {
        bail!("invalid permanent display response");
    }
    let count = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    let max = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    if count > max || count > 4 {
        bail!("invalid permanent display count");
    }
    Ok(count)
}
/// Persistent driver setting, applied only when the administrator explicitly
/// configured this key. It is independent of temporary streaming leases.
pub fn configure_permanent(config: &butterpollo_core::config::Config) -> Result<()> {
    // dd_vdd_static_monitor_count is the key's older name.
    let Some(value) = config
        .values
        .get("dd_virtual_display_permanent_count")
        .or_else(|| config.values.get("dd_vdd_static_monitor_count"))
    else {
        return Ok(());
    };
    let count = value
        .parse::<u32>()
        .context("invalid permanent virtual display count")?;
    if permanent_display_count()? == count {
        return Ok(());
    }
    set_permanent_display_count(count)
}
/// The driver's persistent display count.
pub fn permanent_display_count() -> Result<u32> {
    permanent_response(&Driver::open()?.ioctl(0x907, 1, &[], 80)?)
}
pub fn set_permanent_display_count(count: u32) -> Result<()> {
    let request = permanent_request(count)?;
    let driver = Driver::open()?;
    let result = driver.ioctl(0x906, 3, &request, 80);
    // The driver may persist the count and report a registry-write failure.
    // Confirm runtime state before treating that failure as fatal.
    let after = match result {
        Ok(bytes) => permanent_response(&bytes)?,
        Err(error) => match driver
            .ioctl(0x907, 1, &[], 80)
            .and_then(|bytes| permanent_response(&bytes))
        {
            Ok(actual) if actual == count => actual,
            _ => return Err(error),
        },
    };
    if after != count {
        bail!("driver did not apply the permanent display count");
    }
    Ok(())
}
pub struct VirtualDisplay {
    driver: Driver,
    pub name: String,
    lease: u64,
    id: u64,
    last_feed: Instant,
    mode: (u32, u32, u32),
    options: VirtualOptions,
    generation: u64,
    capability: [u8; 32],
    startup_protection: Option<hotplug::Protection>,
    resolved_target: Option<Monitor>,
    /// Windows left this display off and the host switched it on.
    pub switched_on: bool,
}
// Driver IOCTLs use a thread-safe Windows device handle; shared access is
// serialized by the enclosing mutex, including feed and final teardown.
unsafe impl Send for VirtualDisplay {}
type DisplayLease = std::sync::Arc<std::sync::Mutex<VirtualDisplay>>;
static DISPLAYS: std::sync::Mutex<
    std::collections::BTreeMap<String, std::sync::Weak<std::sync::Mutex<VirtualDisplay>>>,
> = std::sync::Mutex::new(std::collections::BTreeMap::new());
fn display_lease(
    id: &str,
    width: u32,
    height: u32,
    fps: u32,
    options: &VirtualOptions,
) -> Result<DisplayLease> {
    let mut displays = DISPLAYS.lock().unwrap();
    displays.retain(|_, lease| lease.strong_count() != 0);
    if let Some(display) = displays.get(id).and_then(std::sync::Weak::upgrade) {
        if display.lock().unwrap().mode != (width, height, fps) {
            bail!("shared virtual display already uses a different mode");
        }
        return Ok(display);
    }
    let display = std::sync::Arc::new(std::sync::Mutex::new(VirtualDisplay::create_options(
        id,
        width,
        height,
        fps,
        options.clone(),
    )?));
    displays.insert(id.into(), std::sync::Arc::downgrade(&display));
    Ok(display)
}
impl VirtualDisplay {
    pub fn create(stable_id: &str, width: u32, height: u32, fps: u32) -> Result<Self> {
        Self::create_rate(
            stable_id,
            width,
            height,
            fps.checked_mul(1000).context("refresh overflow")?,
        )
    }
    pub fn create_rate(stable_id: &str, width: u32, height: u32, fps: u32) -> Result<Self> {
        Self::create_options(stable_id, width, height, fps, VirtualOptions::default())
    }
    pub fn create_options(
        stable_id: &str,
        width: u32,
        height: u32,
        fps: u32,
        options: VirtualOptions,
    ) -> Result<Self> {
        if !(320..=7680).contains(&width)
            || !(200..=4320).contains(&height)
            || !(1000..=1_000_000).contains(&fps)
        {
            bail!("virtual display mode is outside the driver limits");
        }
        let driver = Driver::open()?;
        let lease = (u64::from_le_bytes(butterpollo_core::crypto::random())
            & 0x1fff_ffff_ffff_ffff)
            | 0x6000_0000_0000_0000;
        let id = butterpollo_core::display_policy::virtual_display_id(stable_id);
        let capability = butterpollo_core::crypto::random::<32>();
        let request = temporary_request(
            lease,
            id,
            (width, height, fps),
            &options,
            driver.protocol,
            &capability,
        );
        let startup_protection = hotplug::Protection::capture()?;
        let result = driver.ioctl(driver.protocol.create_function(), 3, &request, 56)?;
        let mut display = Self {
            driver,
            name: String::new(),
            lease,
            id,
            last_feed: Instant::now(),
            mode: (width, height, fps),
            options,
            generation: 0,
            capability,
            startup_protection: Some(startup_protection),
            resolved_target: None,
            switched_on: false,
        };
        display.resolve(&result)?;
        display.check_hotplug("created")?;
        Ok(display)
    }
    fn resolve(&mut self, result: &[u8]) -> Result<()> {
        if result.len() != 56
            || result[..16] != NAMESPACE
            || result[16..24] != self.lease.to_le_bytes()
            || result[24..32] != self.id.to_le_bytes()
        {
            bail!("invalid virtual display identity response");
        }
        let luid = LUID {
            LowPart: u32::from_le_bytes(result[32..36].try_into().unwrap()),
            HighPart: i32::from_le_bytes(result[36..40].try_into().unwrap()),
        };
        let target = u32::from_le_bytes(result[40..44].try_into().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        // Windows decides whether a display it has just connected joins the
        // desktop. A layout it saved for the same displays (duplicate, or one
        // screen only) can leave the new virtual display connected but off.
        // Vibepollo's display helper switches it on after a short grace; so
        // do we, without changing the other displays.
        let mut connected_since = None;
        let mut next_check = Instant::now();
        while Instant::now() < deadline {
            if let Ok(monitors) = monitors()
                && let Some(m) = monitors
                    .into_iter()
                    .find(|m| m.adapter == luid && m.target == target)
            {
                self.name = m.display_name.clone();
                self.resolved_target = Some(m);
                self.last_feed = Instant::now();
                return Ok(());
            }
            if Instant::now() >= next_check {
                next_check = Instant::now() + Duration::from_millis(250);
                let connected = Topology::query_all().is_ok_and(|all| {
                    all.paths.iter().any(|p| {
                        p.targetInfo.adapterId == luid
                            && p.targetInfo.id == target
                            && p.targetInfo.targetAvailable.as_bool()
                    })
                });
                if connected {
                    let since = *connected_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= Duration::from_secs(1) {
                        next_check = Instant::now() + Duration::from_secs(1);
                        match activate_target(luid, target) {
                            Ok(Some(kept_timings)) => {
                                self.switched_on = true;
                                tracing::info!(
                                    kept_timings,
                                    "Windows left the new virtual display switched off; switched it on beside the current displays"
                                )
                            }
                            Ok(None) => {}
                            Err(error) => tracing::warn!(
                                error = format!("{error:#}"),
                                "could not switch on the new virtual display"
                            ),
                        }
                    }
                }
            }
            if self.last_feed.elapsed() >= Duration::from_secs(1) {
                self.renew()?;
                self.last_feed = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if connected_since.is_some() {
            bail!("virtual display was connected but Windows kept it switched off")
        }
        bail!("virtual display did not become active before the deadline")
    }
    fn owns_monitor(&self, monitor: &Monitor) -> bool {
        self.resolved_target.as_ref().is_some_and(|owned| {
            owned.adapter == monitor.adapter
                && owned.target == monitor.target
                && owned
                    .monitor_device_path
                    .eq_ignore_ascii_case(&monitor.monitor_device_path)
        })
    }
    fn hotplug_monitor(&self) -> Result<&Monitor> {
        // Resolve stores the target returned by our driver request. Never infer
        // ownership from a GDI display name that Windows could later reuse.
        self.resolved_target
            .as_ref()
            .context("owned virtual display unavailable during startup protection")
    }
    fn refresh_name(&mut self) -> Result<()> {
        self.name = monitors()?
            .into_iter()
            .find(|m| self.owns_monitor(m))
            .context("owned virtual display unavailable after startup protection")?
            .display_name;
        Ok(())
    }
    fn check_hotplug(&mut self, stage: &str) -> Result<()> {
        if let Some(protection) = &self.startup_protection {
            if let Err(error) = protection.check(self.hotplug_monitor()?, stage) {
                // Keeping another display off is a courtesy; a stream that
                // cannot start is worse than one that leaves it on.
                tracing::warn!(
                    error = format!("{error:#}"),
                    stage,
                    "could not keep inactive displays off; streaming without startup protection"
                );
                self.startup_protection = None;
            }
            self.refresh_name()?;
        }
        Ok(())
    }
    /// Finish only after virtual HDR settings have settled. Afterwards the
    /// user is free to change displays without a stream overriding them.
    fn finish_hotplug(&mut self, stage: &str) -> Result<()> {
        if self.startup_protection.is_some() {
            let owned = self.hotplug_monitor()?.clone();
            let protection = self.startup_protection.as_mut().unwrap();
            if let Err(error) = protection.settle(&owned, stage) {
                // The guard covers startup only and is a courtesy to the other
                // displays: end it and keep streaming rather than fail.
                tracing::warn!(
                    error = format!("{error:#}"),
                    stage,
                    "virtual display startup protection ended before the layout settled"
                );
            }
            self.refresh_name()?;
            self.startup_protection = None;
        }
        Ok(())
    }
    fn renew(&mut self) -> Result<()> {
        let mut request = NAMESPACE.to_vec();
        request.extend_from_slice(&self.lease.to_le_bytes());
        request.extend_from_slice(&10000u32.to_le_bytes());
        request.extend_from_slice(&0u32.to_le_bytes());
        self.driver.ioctl(0x903, 3, &request, 0)?;
        Ok(())
    }
    pub fn feed(&mut self) -> Result<()> {
        if self.last_feed.elapsed() >= Duration::from_secs(1) {
            self.last_feed = Instant::now();
            let renewed = self.renew();
            let present = Topology::query().map(|topology| {
                self.resolved_target
                    .as_ref()
                    .is_some_and(|owned| topology.shows(owned))
            });
            if renewed.is_ok() && present? {
                return Ok(());
            }
            // Recreate with the same owner lease and stable display ID. The
            // driver can renew an existing owned monitor or recover an expired
            // one, without adopting an unrelated display with the same name.
            self.driver = Driver::open()?;
            let request = temporary_request(
                self.lease,
                self.id,
                self.mode,
                &self.options,
                self.driver.protocol,
                &self.capability,
            );
            let protection = hotplug::Protection::capture()?;
            let response =
                self.driver
                    .ioctl(self.driver.protocol.create_function(), 3, &request, 56)?;
            self.startup_protection = Some(protection);
            self.resolve(&response)?;
            // Retain a resolved recovery even if protection needs a retry, so
            // Guard::feed still completes HDR/startup validation next time.
            self.generation = self.generation.wrapping_add(1);
            self.check_hotplug("recreated")?;
            tracing::info!(output=%self.name, "owned virtual display recovered");
        }
        Ok(())
    }
}
impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        let mut request = NAMESPACE.to_vec();
        request.extend_from_slice(&self.lease.to_le_bytes());
        request.extend_from_slice(&self.id.to_le_bytes());
        if let Err(e) = self.driver.ioctl(0x902, 3, &request, 0) {
            tracing::warn!(error=%e,"virtual display removal failed; lease expiration will recover it");
        }
        request.truncate(24);
        request.extend_from_slice(&0u64.to_le_bytes());
        let _ = self.driver.ioctl(0x904, 3, &request, 0);
    }
}
// Every stream holds a settings lease, including streams that inherit an existing
// HDR mode. Restoration belongs to the last owner, so one client's departure
// cannot change the display underneath another client.
struct Settings {
    users: usize,
    /// (previous, applied).
    mode: Option<(Timing, Timing)>,
    color: Option<(Monitor, bool, bool)>,
}
/// Width, height and refresh in millihertz.
type Timing = (u32, u32, u32);
static SETTINGS: std::sync::Mutex<std::collections::BTreeMap<String, Settings>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());
/// The displays streams, paused game displays, launches being prepared and
/// remote monitors hold now, by device id.
pub fn leased_displays() -> Vec<String> {
    SETTINGS.lock().unwrap().keys().cloned().collect()
}
/// Whether a stream may change a display's mode or HDR. Only its sole user
/// may: a second stream keeps what the first one streams, whether or not
/// the first changed anything, and the encoder scales.
#[derive(Debug, PartialEq)]
enum Plan {
    Keep,
    Change,
}
/// `recorded`: the value an earlier stream applied to this display.
fn plan<T: PartialEq>(users: usize, recorded: Option<&T>, current: &T, requested: &T) -> Plan {
    if users > 1 || recorded.is_some() || current == requested {
        Plan::Keep
    } else {
        Plan::Change
    }
}
/// What a stream's HDR request needs from a display. One without HDR is
/// left alone and the stream continues in SDR, as libdisplaydevice leaves
/// a display without an HDR state: the launch must not fail on it.
#[derive(Debug, PartialEq)]
enum HdrAction {
    Keep,
    Skip,
    Set(bool),
}
fn hdr_action(supported: bool, current: bool, requested: Option<bool>) -> HdrAction {
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
fn restore_hdr(
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
/// What a stream that leaves a setting as it is restores at its end: a
/// pending journal original, when the display no longer has it. An earlier
/// stream ended while the display was away (a TV in standby) and left its
/// change behind; without this the release would clear that entry.
fn adopt<T: PartialEq + Copy>(pending: Option<T>, current: T) -> Option<(T, T)> {
    pending
        .filter(|original| *original != current)
        .map(|original| (original, current))
}
/// Take a stream's part in a display's HDR: change it when the stream is
/// its sole user and the display can, and record what teardown restores.
/// `pending`: the journal's original, which wins over the current state.
fn lease_hdr(
    settings: &mut Settings,
    chosen: &Monitor,
    requested: Option<bool>,
    pending: Option<bool>,
    journal: impl FnOnce(bool, bool) -> Result<()>,
    set: impl FnOnce(&Monitor, bool) -> Result<()>,
) -> Result<()> {
    match hdr_action(chosen.hdr_supported, chosen.hdr_enabled, requested) {
        HdrAction::Keep => {}
        HdrAction::Skip => tracing::warn!(
            output = %chosen.display_name,
            "Display does not support HDR; capturing SDR content even if the wire stream is HDR. Use an HDR virtual display or disable HDR in the client"
        ),
        HdrAction::Set(enabled) => {
            let applied = settings.color.as_ref().map(|(_, _, applied)| applied);
            if plan(settings.users, applied, &chosen.hdr_enabled, &enabled) == Plan::Change {
                journal(chosen.hdr_enabled, enabled)?;
                let previous = pending.unwrap_or(chosen.hdr_enabled);
                settings.color = Some((chosen.clone(), previous, enabled));
                return set(chosen, enabled);
            }
            tracing::warn!(
                output = %chosen.display_name,
                hdr = chosen.hdr_enabled,
                "Another stream uses this display; keeping its HDR state. Use a separate virtual display or match the other stream HDR setting"
            );
        }
    }
    if settings.color.is_none() {
        settings.color = adopt(pending, chosen.hdr_enabled)
            .map(|(previous, applied)| (chosen.clone(), previous, applied));
    }
    Ok(())
}
/// Undo what a display's streams changed. False when its journal entry must
/// stay: a physical display that is gone gets its settings back when it
/// returns (Windows remembers them per display), and recovery retries a
/// failed restore.
fn restore_settings(
    settings: &Settings,
    identity: &str,
    output: &str,
    is_virtual: bool,
    now: Option<&[Monitor]>,
    set_hdr: impl FnOnce(&Monitor, bool) -> Result<()>,
    restore_mode: impl FnOnce(Timing, Timing) -> Result<()>,
) -> bool {
    let mut restored = true;
    let present = now.is_some_and(|all| all.iter().any(|m| m.device_id == identity));
    if !present && !is_virtual && (settings.color.is_some() || settings.mode.is_some()) {
        restored = false;
        tracing::info!(output = %output, "display is gone; its settings are restored when it returns");
    }
    if let Some((monitor, previous, applied)) = &settings.color
        && let Some(now) = now
        && let Err(e) = restore_hdr(monitor, *previous, *applied, now, set_hdr)
    {
        restored = false;
        tracing::warn!(error=%e,"HDR restoration failed");
    }
    if let Some((previous, applied)) = settings.mode
        && let Err(e) = restore_mode(previous, applied)
    {
        restored = false;
        tracing::warn!(error=%e,"display mode restoration failed");
    }
    restored
}
/// Set the previous mode back if the display still has the one applied.
fn restore_mode_rate(
    output: &str,
    identity: &str,
    previous: Timing,
    applied: Timing,
) -> Result<()> {
    if mode(output).is_ok_and(|m| (m.dmPelsWidth, m.dmPelsHeight) == (applied.0, applied.1))
        && Topology::query()
            .and_then(|t| t.refresh(identity))
            .is_ok_and(|r| r.0 == applied.2)
    {
        Topology::set_mode_rate(
            output,
            previous.0,
            previous.1,
            butterpollo_core::framegen::Rate(previous.2),
        )?;
    }
    Ok(())
}
pub struct Guard {
    pub output: String,
    virtual_display: Option<DisplayLease>,
    identity: String,
    generation: u64,
    hdr: Option<bool>,
}
/// Every mode a display reports as (width, height, refresh Hz).
fn supported_modes(output: &str) -> Vec<(u32, u32, u32)> {
    let name: Vec<_> = output.encode_utf16().chain(Some(0)).collect();
    let mut modes = Vec::new();
    for index in 0..4096 {
        let mut candidate = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
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
impl Guard {
    #[allow(clippy::too_many_arguments)] // Native display lease parameters.
    pub fn new(
        output: &str,
        virtual_mode: bool,
        stable_id: &str,
        width: u32,
        height: u32,
        fps: u32,
        hdr: bool,
        physical_resolution: Option<(u32, u32)>,
        physical_refresh: Option<u32>,
    ) -> Result<Self> {
        Self::new_options(
            output,
            virtual_mode,
            stable_id,
            width,
            height,
            butterpollo_core::framegen::Rate(fps.checked_mul(1000).context("refresh overflow")?),
            Some(hdr),
            physical_resolution,
            physical_refresh.map(|r| butterpollo_core::framegen::Rate(r.saturating_mul(1000))),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn new_options(
        output: &str,
        virtual_mode: bool,
        stable_id: &str,
        width: u32,
        height: u32,
        rate: butterpollo_core::framegen::Rate,
        hdr: Option<bool>,
        physical_resolution: Option<(u32, u32)>,
        physical_refresh: Option<butterpollo_core::framegen::Rate>,
    ) -> Result<Self> {
        Self::new_virtual_options(
            output,
            virtual_mode,
            stable_id,
            width,
            height,
            rate,
            hdr,
            physical_resolution,
            physical_refresh,
            &VirtualOptions::default(),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn new_virtual_options(
        output: &str,
        virtual_mode: bool,
        stable_id: &str,
        width: u32,
        height: u32,
        rate: butterpollo_core::framegen::Rate,
        hdr: Option<bool>,
        physical_resolution: Option<(u32, u32)>,
        physical_refresh: Option<butterpollo_core::framegen::Rate>,
        options: &VirtualOptions,
    ) -> Result<Self> {
        let virtual_display = if virtual_mode {
            Some(display_lease(stable_id, width, height, rate.0, options)?)
        } else {
            None
        };
        let choices = monitors()?;
        let chosen = if let Some(v) = &virtual_display {
            let display = v.lock().unwrap();
            choices.iter().find(|d| display.owns_monitor(d))
        } else {
            choices.iter().find(|d| d.matches(output)).or_else(|| {
                if output.is_empty() {
                    choices
                        .iter()
                        .find(|d| d.primary)
                        .or_else(|| choices.first())
                } else {
                    None
                }
            })
        }
        .context("display unavailable")?
        .clone();
        let mut guard = Self {
            output: chosen.display_name.clone(),
            generation: virtual_display
                .as_ref()
                .map_or(0, |v| v.lock().unwrap().generation),
            hdr,
            virtual_display,
            identity: chosen.device_id.clone(),
        };
        let result = (|| -> Result<()> {
            let mut all = SETTINGS.lock().unwrap();
            let settings = all.entry(guard.identity.clone()).or_insert(Settings {
                users: 0,
                mode: None,
                color: None,
            });
            settings.users += 1;
            // The first user takes over what an earlier stream left pending
            // on this display, so that its original values come back.
            let pending = if settings.users == 1 {
                crate::display_recovery::pending(&guard.identity)
            } else {
                Default::default()
            };
            if (physical_resolution.is_some() || physical_refresh.is_some())
                && guard.virtual_display.is_none()
            {
                let previous = mode(&guard.output)?;
                let previous_rate = Topology::query()?.refresh(&guard.identity)?;
                let current = (previous.dmPelsWidth, previous.dmPelsHeight, previous_rate.0);
                let supported = supported_modes(&guard.output);
                let requested = if supported.is_empty() {
                    let (width, height) = physical_resolution
                        .unwrap_or((previous.dmPelsWidth, previous.dmPelsHeight));
                    (width, height, physical_refresh.unwrap_or(previous_rate).0)
                } else {
                    butterpollo_core::display_policy::physical_mode(
                        &supported,
                        (physical_resolution, physical_refresh.map(|rate| rate.0)),
                        current,
                    )
                };
                if let Some((width, height)) = physical_resolution
                    && (width, height) != (requested.0, requested.1)
                {
                    tracing::info!(
                        output = %guard.output,
                        "display has no {width}x{height} mode; keeping {}x{} and scaling the stream",
                        requested.0,
                        requested.1
                    );
                }
                let (width, height, fps) = requested;
                let applied = settings.mode.as_ref().map(|(_, applied)| applied);
                if plan(settings.users, applied, &current, &requested) == Plan::Keep {
                    // Another stream uses this display; keep its mode and let
                    // the encoder scale, rather than refuse the second stream.
                    let kept = *applied.unwrap_or(&current);
                    if kept != requested {
                        tracing::info!(
                            output = %guard.output,
                            "another stream uses this display at {}x{}; scaling this stream",
                            kept.0,
                            kept.1
                        );
                    }
                } else {
                    let original = pending.mode.map_or(current, |(original, _)| original);
                    crate::display_recovery::mode_rate(
                        &guard.identity,
                        &guard.output,
                        current,
                        requested,
                    )?;
                    settings.mode = Some((original, requested));
                    let applied = Topology::set_mode_rate(
                        &guard.output,
                        width,
                        height,
                        butterpollo_core::framegen::Rate(fps),
                    );
                    // Record what the display has now even when the rate
                    // was refused: a TV can take the resolution and keep
                    // its old rate, and teardown restores only the mode
                    // it finds recorded.
                    let actual = mode(&guard.output)?;
                    let actual_rate = Topology::query()?.refresh(&guard.identity)?;
                    let actual_mode = (actual.dmPelsWidth, actual.dmPelsHeight, actual_rate.0);
                    crate::display_recovery::mode_rate(
                        &guard.identity,
                        &guard.output,
                        current,
                        actual_mode,
                    )?;
                    settings.mode = Some((original, actual_mode));
                    applied?;
                }
            }
            // A stream that leaves the mode as it is, or requests none, takes
            // the earlier stream's change over: its end restores the original
            // if the display still has that change, as recovery would.
            if settings.mode.is_none() && guard.virtual_display.is_none() {
                settings.mode = pending
                    .mode
                    .filter(|(original, applied)| original != applied);
            }
            lease_hdr(
                settings,
                &chosen,
                hdr,
                pending.hdr,
                |before, applied| {
                    crate::display_recovery::hdr(&guard.identity, &guard.output, before, applied)
                },
                set_hdr,
            )
        })();
        result?;
        if let Some(display) = &guard.virtual_display {
            let mut display = display.lock().unwrap();
            display.finish_hotplug("after settings")?;
            guard.output = display.name.clone();
        }
        Ok(guard)
    }
    pub fn feed(&mut self) -> Result<bool> {
        if let Some(display) = &mut self.virtual_display {
            let mut display = display.lock().unwrap();
            display.feed()?;
            if display.generation != self.generation {
                let chosen = monitors()?
                    .into_iter()
                    .find(|m| display.owns_monitor(m))
                    .context("recovered display unavailable")?;
                let mut settings = SETTINGS.lock().unwrap();
                if chosen.device_id != self.identity {
                    if let Some(old) = settings.get_mut(&self.identity) {
                        old.users -= 1;
                        if old.users == 0 {
                            settings.remove(&self.identity);
                        }
                    }
                    if !settings.contains_key(&self.identity) {
                        crate::display_recovery::release(&self.identity)?;
                    }
                    settings
                        .entry(chosen.device_id.clone())
                        .or_insert(Settings {
                            users: 0,
                            mode: None,
                            color: None,
                        })
                        .users += 1;
                    self.identity = chosen.device_id.clone();
                }
                if let Some(hdr) = self.hdr {
                    if let Some(color) = settings
                        .get_mut(&self.identity)
                        .and_then(|s| s.color.as_mut())
                    {
                        color.0 = chosen.clone();
                    }
                    if chosen.hdr_enabled != hdr {
                        crate::display_recovery::hdr(
                            &self.identity,
                            &chosen.display_name,
                            chosen.hdr_enabled,
                            hdr,
                        )?;
                        let state = settings.get_mut(&self.identity).unwrap();
                        if state.color.is_none() {
                            state.color = Some((chosen.clone(), chosen.hdr_enabled, hdr));
                        }
                        set_hdr(&chosen, hdr)?;
                    }
                }
                display.finish_hotplug("after recovery settings")?;
                self.output = display.name.clone();
                self.generation = display.generation;
                return Ok(true);
            }
        }
        Ok(false)
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let mut all = SETTINGS.lock().unwrap();
        let Some(settings) = all.get_mut(&self.identity) else {
            return;
        };
        settings.users -= 1;
        if settings.users != 0 {
            return;
        }
        let settings = all.remove(&self.identity).unwrap();
        // A physical display that is off or unplugged now keeps its journal
        // entry: Windows remembers HDR and modes per display, and the next
        // stream on it or host start restores them once it is back.
        let restored = restore_settings(
            &settings,
            &self.identity,
            &self.output,
            self.virtual_display.is_some(),
            monitors().ok().as_deref(),
            set_hdr,
            |previous, applied| restore_mode_rate(&self.output, &self.identity, previous, applied),
        );
        if restored && let Err(e) = crate::display_recovery::release(&self.identity) {
            tracing::warn!(error=%e,"display recovery journal cleanup failed");
        }
    }
}

/// A monitor lease outlives a streaming session. The feeder owns only the guard,
/// so dropping the final lease can always stop and join it before removing VDD.
pub struct Retained {
    pub output: String,
    pub mode: (u32, u32, u32, bool),
    guard: std::sync::Arc<std::sync::Mutex<Option<Guard>>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Retained {
    pub fn capture_target(&self) -> (String, u64) {
        self.guard.lock().unwrap().as_ref().map_or_else(
            || (self.output.clone(), 0),
            |guard| (guard.output.clone(), guard.generation),
        )
    }
    pub fn current_output(&self) -> String {
        self.guard
            .lock()
            .unwrap()
            .as_ref()
            .map_or_else(|| self.output.clone(), |g| g.output.clone())
    }
    pub fn current_generation(&self) -> u64 {
        self.guard
            .lock()
            .unwrap()
            .as_ref()
            .map_or(0, |g| g.generation)
    }
    pub fn create(id: &str, width: u32, height: u32, fps: u32, hdr: bool) -> Result<Self> {
        Self::create_rate(
            id,
            width,
            height,
            butterpollo_core::framegen::Rate(fps.checked_mul(1000).context("refresh overflow")?),
            hdr,
        )
    }
    pub fn create_rate(
        id: &str,
        width: u32,
        height: u32,
        rate: butterpollo_core::framegen::Rate,
        hdr: bool,
    ) -> Result<Self> {
        Self::create_options(
            id,
            width,
            height,
            rate,
            hdr,
            &VirtualOptions::default(),
            None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn create_options(
        id: &str,
        width: u32,
        height: u32,
        rate: butterpollo_core::framegen::Rate,
        hdr: bool,
        options: &VirtualOptions,
        on_recovery: Option<Box<dyn Fn() -> Result<()> + Send>>,
    ) -> Result<Self> {
        let guard = Guard::new_virtual_options(
            "",
            true,
            id,
            width,
            height,
            rate,
            Some(hdr),
            None,
            None,
            options,
        )?;
        let output = guard.output.clone();
        let guard = std::sync::Arc::new(std::sync::Mutex::new(Some(guard)));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_guard = guard.clone();
        let worker_stop = stop.clone();
        let worker = std::thread::Builder::new()
            .name("monitor-lease".into())
            .spawn(move || {
                let mut pending = false;
                let mut retry_at = Instant::now();
                while !worker_stop.load(std::sync::atomic::Ordering::Acquire) {
                    let result = worker_guard
                        .lock()
                        .unwrap()
                        .as_mut()
                        .map(Guard::feed)
                        .transpose();
                    match result {
                        Ok(Some(true)) => {
                            pending = true;
                            retry_at = Instant::now();
                        }
                        Err(e) => {
                            tracing::warn!(error=%e,"retained monitor lease heartbeat failed")
                        }
                        _ => {}
                    }
                    if pending && Instant::now() >= retry_at {
                        retry_at = Instant::now() + Duration::from_secs(1);
                        match on_recovery.as_ref().map(|callback| callback()).transpose() {
                            Ok(_) => pending = false,
                            Err(error) => {
                                tracing::warn!(%error, "retained monitor layout recovery failed")
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            })?;
        Ok(Self {
            output,
            mode: (width, height, rate.0, hdr),
            guard,
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for Retained {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.guard.lock().unwrap().take();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn temporary_monitor_metadata_preserves_the_owned_identity_and_sanitizes_labels() {
        let options = VirtualOptions {
            label: "Living Room\0\n😀   ".into(),
            peak_nits: 1500,
        };
        let request = temporary_request(
            17,
            23,
            (1920, 1080, 59940),
            &options,
            DriverProtocol::Secure36,
            &[42; 32],
        );
        assert_eq!(request.len(), 128);
        assert_eq!(&request[16..24], &17u64.to_le_bytes());
        assert_eq!(&request[24..32], &23u64.to_le_bytes());
        assert_eq!(&request[48..52], &59940u32.to_le_bytes());
        assert_eq!(&request[56..88], &display_label("Living Room"));
        assert_eq!(&request[88..92], &0u32.to_le_bytes());
        assert_eq!(&request[92..96], &1500u32.to_le_bytes());
        assert_eq!(&display_label("😀")[..11], b"Butterpollo");
        assert_eq!(display_label(&"A".repeat(40))[31], 0);
    }
    #[test]
    fn heartbeat_check_finds_the_monitors_that_monitors_lists() -> Result<()> {
        let topology = Topology::query()?;
        for monitor in monitors()? {
            assert!(topology.shows(&monitor), "{}", monitor.monitor_device_path);
            let other = Monitor {
                monitor_device_path: format!("{}#other", monitor.monitor_device_path),
                ..monitor.clone()
            };
            assert!(!topology.shows(&other));
            let moved = Monitor {
                target: monitor.target ^ 0x8000_0000,
                ..monitor
            };
            assert!(!topology.shows(&moved));
        }
        Ok(())
    }
    #[test]
    fn only_a_displays_sole_stream_changes_its_mode_or_hdr() {
        let (current, requested) = ((3840, 2160, 60000), (1920, 1080, 120000));
        assert_eq!(plan(1, None, &current, &requested), Plan::Change);
        assert_eq!(plan(1, None, &current, &current), Plan::Keep);
        // The first stream needed no change: the second keeps its mode.
        assert_eq!(plan(2, None, &current, &requested), Plan::Keep);
        assert_eq!(plan(2, Some(&current), &current, &requested), Plan::Keep);
        assert_eq!(plan(2, None, &false, &true), Plan::Keep);
        assert_eq!(plan(1, None, &false, &true), Plan::Change);
    }
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
    fn monitor(id: &str, adapter: u32, hdr: bool) -> Monitor {
        Monitor {
            device_id: id.into(),
            monitor_device_path: String::new(),
            display_name: r"\\.\DISPLAY1".into(),
            friendly_name: id.into(),
            hdr_supported: true,
            hdr_enabled: hdr,
            primary: false,
            adapter: LUID {
                LowPart: adapter,
                HighPart: 0,
            },
            target: 7,
            source: 0,
        }
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
    #[test]
    fn a_pending_original_comes_back_rather_than_being_cleared() -> Result<()> {
        // A stream turned the TV's HDR on and ended while it was in standby:
        // the journal kept the entry and the TV came back with HDR on.
        let directory = tempfile::tempdir()?;
        let journal = directory.path().join("display-recovery.json");
        std::fs::write(
            &journal,
            serde_json::to_vec(&serde_json::json!({
                "pid": 0,
                "started": 0,
                "entries": {"tv": {"output": r"\\.\DISPLAY1", "hdr": [false, true]}},
            }))?,
        )?;
        let pending = crate::display_recovery::pending_in(&journal, "tv")?;
        // The next stream wants HDR too, so it changes nothing.
        let mut settings = Settings {
            users: 1,
            mode: None,
            color: None,
        };
        let unchanged = |_: &Monitor, _| -> Result<()> { panic!("HDR is already on") };
        lease_hdr(
            &mut settings,
            &monitor("tv", 1, true),
            Some(true),
            pending.hdr,
            |_, _| panic!("nothing to journal"),
            unchanged,
        )?;
        let no_mode = |_, _| -> Result<()> { panic!("no mode recorded") };
        // Away again at its end: the entry stays for recovery.
        assert!(!restore_settings(
            &settings,
            "tv",
            "TV",
            false,
            Some(&[]),
            unchanged,
            no_mode
        ));
        assert_eq!(
            crate::display_recovery::pending_in(&journal, "tv")?,
            pending
        );
        // Present: the original comes back, then the entry is released.
        let mut restored = None;
        assert!(restore_settings(
            &settings,
            "tv",
            "TV",
            false,
            Some(&[monitor("tv", 2, true)]),
            |_, enabled| {
                restored = Some(enabled);
                Ok(())
            },
            no_mode
        ));
        assert_eq!(restored, Some(false));
        crate::display_recovery::release_in(&journal, "tv")?;
        assert_eq!(
            crate::display_recovery::pending_in(&journal, "tv")?,
            Default::default()
        );
        // Without a pending entry a stream that changes nothing records nothing.
        let mut settings = Settings {
            users: 1,
            mode: None,
            color: None,
        };
        lease_hdr(
            &mut settings,
            &monitor("tv", 1, true),
            Some(true),
            None,
            |_, _| panic!("nothing to journal"),
            unchanged,
        )?;
        assert!(settings.color.is_none());
        Ok(())
    }
    #[test]
    fn a_stream_that_changes_a_setting_restores_the_pending_original() -> Result<()> {
        // The TV was left in HDR; this stream wants SDR.
        let mut settings = Settings {
            users: 1,
            mode: None,
            color: None,
        };
        let mut journaled = None;
        let mut set = None;
        lease_hdr(
            &mut settings,
            &monitor("tv", 1, true),
            Some(false),
            Some(false),
            |before, applied| {
                journaled = Some((before, applied));
                Ok(())
            },
            |_, enabled| {
                set = Some(enabled);
                Ok(())
            },
        )?;
        assert_eq!((journaled, set), (Some((true, false)), Some(false)));
        let (_, previous, applied) = settings.color.unwrap();
        assert_eq!((previous, applied), (false, false));
        // Left as it is: the original wins, unless the display has it.
        assert_eq!(adopt(Some(false), true), Some((false, true)));
        assert_eq!(adopt(Some(false), false), None);
        assert_eq!(adopt(None, true), None);
        Ok(())
    }
    #[test]
    fn extending_a_cloned_desktop_assigns_distinct_sources_and_rejects_impossible_routes() {
        let path = |source, target| {
            let mut path = DISPLAYCONFIG_PATH_INFO::default();
            path.sourceInfo.id = source;
            path.targetInfo.id = target;
            path
        };
        let result = extended_paths(&[vec![path(0, 10), path(1, 10)], vec![path(0, 20)]]).unwrap();
        assert_eq!(result[0].sourceInfo.id, 1);
        assert_eq!(result[1].sourceInfo.id, 0);
        assert!(extended_paths(&[vec![path(0, 10)], vec![path(0, 20)]]).is_err());
    }
    #[test]
    fn a_display_leaving_a_duplicate_group_is_moved_but_its_partner_and_new_displays_are_not() {
        let path = |source, target, active: bool| {
            let mut path = DISPLAYCONFIG_PATH_INFO::default();
            path.sourceInfo.id = source;
            path.targetInfo.id = target;
            path.flags = if active { DISPLAYCONFIG_PATH_ACTIVE } else { 0 };
            path
        };
        // Targets 10 and 11 duplicate source 1; target 12 is off.
        let all = [
            path(1, 10, true),
            path(1, 11, true),
            path(0, 10, false),
            path(0, 11, false),
            path(2, 12, false),
        ];
        let chosen = [path(0, 10, false), path(1, 11, true), path(2, 12, false)];
        assert_eq!(moving_targets(&chosen, &all), [true, false, false]);
    }
    #[test]
    fn displays_that_stay_on_keep_their_modes_when_the_layout_changes() {
        let path = |source, target, mode: u32, active: bool| {
            let mut path = DISPLAYCONFIG_PATH_INFO::default();
            path.sourceInfo.id = source;
            path.targetInfo.id = target;
            path.sourceInfo.Anonymous.modeInfoIdx = mode;
            path.targetInfo.Anonymous.modeInfoIdx = mode + 1;
            path.flags = if active { DISPLAYCONFIG_PATH_ACTIVE } else { 0 };
            path.targetInfo.refreshRate = DISPLAYCONFIG_RATIONAL {
                Numerator: 120,
                Denominator: 1,
            };
            path.targetInfo.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
            path
        };
        // An inactive route from QDC_ALL_PATHS has no rotation or scaling.
        let kept = keep_active_modes(&[path(0, 10, 4, true), path(1, 11, 6, false)]);
        assert!(kept.iter().all(|p| p.flags == DISPLAYCONFIG_PATH_ACTIVE));
        assert_eq!(unsafe { kept[0].sourceInfo.Anonymous.modeInfoIdx }, 4);
        assert_eq!(unsafe { kept[0].targetInfo.Anonymous.modeInfoIdx }, 5);
        assert_eq!(
            unsafe { kept[1].sourceInfo.Anonymous.modeInfoIdx },
            DISPLAYCONFIG_PATH_MODE_IDX_INVALID
        );
        assert_eq!(
            unsafe { kept[1].targetInfo.Anonymous.modeInfoIdx },
            DISPLAYCONFIG_PATH_MODE_IDX_INVALID
        );
        // The display that stays on keeps its timing; Windows chooses the
        // switched-on one's, which needs valid rotation and scaling.
        assert_eq!(kept[0].targetInfo.refreshRate.Numerator, 120);
        assert_eq!(kept[1].targetInfo.refreshRate.Denominator, 0);
        assert_eq!(
            kept[1].targetInfo.scanLineOrdering,
            DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED
        );
        assert_eq!(kept[1].targetInfo.rotation, DISPLAYCONFIG_ROTATION_IDENTITY);
        assert_eq!(kept[1].targetInfo.scaling, DISPLAYCONFIG_SCALING_PREFERRED);
    }
    #[test]
    fn switching_on_a_virtual_display_keeps_duplicated_tvs_and_their_modes() {
        let gpu = LUID {
            LowPart: 1,
            HighPart: 0,
        };
        let vdd = LUID {
            LowPart: 9,
            HighPart: 0,
        };
        let path = |adapter, source, target, mode: u32, active, available: bool| {
            let mut path = DISPLAYCONFIG_PATH_INFO::default();
            path.sourceInfo.adapterId = adapter;
            path.sourceInfo.id = source;
            path.sourceInfo.Anonymous.modeInfoIdx = mode;
            path.targetInfo.adapterId = adapter;
            path.targetInfo.id = target;
            path.targetInfo.Anonymous.modeInfoIdx = mode + 1;
            path.targetInfo.targetAvailable = available.into();
            path.targetInfo.refreshRate = DISPLAYCONFIG_RATIONAL {
                Numerator: 120,
                Denominator: 1,
            };
            path.targetInfo.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
            path.flags = if active { DISPLAYCONFIG_PATH_ACTIVE } else { 0 };
            path
        };
        // Two TVs share GPU source 0 (duplicate). The virtual display's own
        // adapter source 0 already drives another client's display.
        let active = [
            path(gpu, 0, 10, 0, true, true),
            path(gpu, 0, 11, 0, true, true),
            path(vdd, 0, 1, 2, true, true),
        ];
        let mut all = active.to_vec();
        all.extend([
            path(gpu, 1, 10, 0, false, true),
            path(vdd, 0, 2, 0, false, true),
            path(vdd, 1, 2, 0, false, true),
        ]);
        let paths = activation_paths(&active, &all, vdd, 2).unwrap().unwrap();
        assert_eq!(paths.len(), 4);
        for (kept, original) in paths.iter().zip(&active) {
            assert_eq!(kept.targetInfo.id, original.targetInfo.id);
            assert_eq!(kept.sourceInfo.id, original.sourceInfo.id);
            assert_eq!(unsafe { kept.sourceInfo.Anonymous.modeInfoIdx }, unsafe {
                original.sourceInfo.Anonymous.modeInfoIdx
            });
            assert_eq!(unsafe { kept.targetInfo.Anonymous.modeInfoIdx }, unsafe {
                original.targetInfo.Anonymous.modeInfoIdx
            });
        }
        let new = paths[3];
        assert_eq!((new.targetInfo.adapterId, new.targetInfo.id), (vdd, 2));
        assert_eq!(new.sourceInfo.id, 1);
        assert_eq!(new.flags, DISPLAYCONFIG_PATH_ACTIVE);
        assert_eq!(
            unsafe { new.sourceInfo.Anonymous.modeInfoIdx },
            DISPLAYCONFIG_PATH_MODE_IDX_INVALID
        );
        assert_eq!(
            unsafe { new.targetInfo.Anonymous.modeInfoIdx },
            DISPLAYCONFIG_PATH_MODE_IDX_INVALID
        );
        assert_eq!(new.targetInfo.refreshRate.Denominator, 0);
        assert_eq!(
            new.targetInfo.scanLineOrdering,
            DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED
        );
        assert_eq!(new.targetInfo.rotation, DISPLAYCONFIG_ROTATION_IDENTITY);
        assert_eq!(new.targetInfo.scaling, DISPLAYCONFIG_SCALING_PREFERRED);
        // Not connected yet: keep waiting rather than failing.
        assert!(activation_paths(&active, &all, vdd, 3).unwrap().is_none());
        let mut gone = all.clone();
        gone.retain(|p| p.targetInfo.id != 2);
        gone.push(path(vdd, 0, 2, 0, false, false));
        assert!(activation_paths(&active, &gone, vdd, 2).unwrap().is_none());
        // Every source of the adapter is in use.
        let busy = [
            active[0],
            active[1],
            active[2],
            path(vdd, 1, 5, 4, true, true),
        ];
        assert!(activation_paths(&busy, &all, vdd, 2).is_err());
    }
    #[test]
    #[ignore = "creates a virtual display and briefly switches it off"]
    fn native_virtual_display_left_off_by_windows_is_switched_on_without_retiming_others()
    -> Result<()> {
        let display = VirtualDisplay::create(
            &format!("activation-test-{}", std::process::id()),
            1280,
            720,
            60,
        )?;
        let owned = display
            .resolved_target
            .clone()
            .context("virtual display unresolved")?;
        let ours = |m: &Monitor| m.adapter == owned.adapter && m.target == owned.target;
        // Device, width, height, refresh and desktop position.
        type Timing = (String, u32, u32, u32, i32, i32);
        let timings = || -> Result<Vec<Timing>> {
            monitors()?
                .iter()
                .filter(|m| !ours(m))
                .map(|m| {
                    let mode = mode(&m.display_name)?;
                    let position = unsafe { mode.Anonymous1.Anonymous2.dmPosition };
                    Ok((
                        m.device_id.clone(),
                        mode.dmPelsWidth,
                        mode.dmPelsHeight,
                        mode.dmDisplayFrequency,
                        position.x,
                        position.y,
                    ))
                })
                .collect()
        };
        let before = timings()?;
        // What Windows does when a saved layout leaves the new display off.
        let active = Topology::query()?;
        let off: Vec<_> = active
            .paths
            .iter()
            .filter(|p| {
                !(p.targetInfo.adapterId == owned.adapter && p.targetInfo.id == owned.target)
            })
            .copied()
            .collect();
        check(unsafe {
            SetDisplayConfig(
                Some(&off),
                Some(&active.modes),
                SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES,
            )
        })?;
        anyhow::ensure!(
            !monitors()?.iter().any(ours),
            "virtual display stayed on after switching it off"
        );
        let kept = activate_target(owned.adapter, owned.target)?;
        anyhow::ensure!(monitors()?.iter().any(ours), "virtual display stayed off");
        assert_eq!(kept, Some(true));
        assert_eq!(timings()?, before);
        println!(
            "switched on {} beside {} displays",
            display.name,
            before.len()
        );
        Ok(())
    }
    #[test]
    fn previous_golden_snapshot_import_preserves_fractional_rates_clones_hdr_and_origins() {
        let document = serde_json::json!({
            "topology":[["a","b"]],"primary":"a",
            "modes":{"a":{"w":1920,"h":1080,"num":60000,"den":1001},"b":{"w":1920,"h":1080,"num":60000,"den":1001}},
            "hdr":{"a":"on","b":"off"}, "origins":{"a":{"x":-1920,"y":0},"b":{"x":-1920,"y":0}},
            "layouts":{"a":{"rotation":90},"b":{"rotation":0}}
        });
        let snapshot = Snapshot::decode(&document).unwrap();
        assert!((snapshot.nodes[0].mode.refresh_hz - 59.94005994).abs() < 1e-8);
        assert_eq!(snapshot.nodes[0].desired_position.x, -1920);
        assert!(snapshot.nodes[0].primary);
        assert_eq!(
            snapshot.clone_groups,
            [vec!["a".to_owned(), "b".to_owned()]]
        );
        assert!(snapshot.hdr["a"]);
        assert!(!snapshot.hdr["b"]);
        assert_eq!(snapshot.rotation["a"], 2);
        let mut malformed = document.clone();
        malformed["modes"]["a"]["den"] = 0.into();
        assert!(Snapshot::decode(&malformed).is_err());
        assert!(Snapshot::decode(&serde_json::json!({"version":2,"nodes":[],"hdr":{}})).is_err());
    }
    #[test]
    fn legacy_and_secure_driver_requests_keep_identity_and_capability() -> Result<()> {
        let mut version = NAMESPACE.to_vec();
        version.extend_from_slice(&[3, 0, 5, 0, 0, 0, 0, 0]);
        assert_eq!(DriverProtocol::parse(&version)?, DriverProtocol::Legacy35);
        let options = VirtualOptions::default();
        let legacy = temporary_request(
            17,
            23,
            (1968, 2184, 120000),
            &options,
            DriverProtocol::Legacy35,
            &[42; 32],
        );
        assert_eq!(legacy.len(), 96);
        assert_eq!(&legacy[88..96], &[0; 8]);
        assert_eq!(DriverProtocol::Legacy35.create_function(), 0x901);
        version[18] = 6;
        assert_eq!(DriverProtocol::parse(&version)?, DriverProtocol::Secure36);
        let first = temporary_request(
            17,
            23,
            (1968, 2184, 120000),
            &options,
            DriverProtocol::Secure36,
            &[42; 32],
        );
        let recovered = temporary_request(
            17,
            23,
            (1968, 2184, 120000),
            &options,
            DriverProtocol::Secure36,
            &[42; 32],
        );
        assert_eq!(&first[..88], &legacy[..88]);
        assert_eq!(&first[96..], &[42; 32]);
        assert_eq!(first, recovered);
        version[18] = 4;
        assert!(DriverProtocol::parse(&version).is_err());
        version[16] = 4;
        assert!(DriverProtocol::parse(&version).is_err());
        Ok(())
    }
    #[test]
    fn permanent_monitor_payload_matches_driver_v3_contract() {
        let request = permanent_request(4).unwrap();
        assert_eq!(request.len(), 76); // SDK PermanentDisplayCountRequest.
        assert_eq!(&request[..16], &NAMESPACE);
        assert_eq!(&request[16..20], &[4, 0, 0, 0]);
        assert_eq!(&request[24..32], &[128, 7, 0, 0, 56, 4, 0, 0]); // 1920 x 1080.
        assert_eq!(&request[40..44], &[96, 234, 0, 0]); // 60000 millihertz.
        assert!(permanent_request(5).is_err());
        let mut response = NAMESPACE.to_vec();
        response.extend_from_slice(&[4, 0, 0, 0, 4, 0, 0, 0]);
        response.resize(80, 0); // SDK PermanentDisplayCountResult.
        assert_eq!(permanent_response(&response).unwrap(), 4);
        response[16] = 5;
        assert!(permanent_response(&response).is_err());
        response[0] ^= 1;
        assert!(permanent_response(&response).is_err());
    }
}
