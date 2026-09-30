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
    pub display_name: String,
    pub friendly_name: String,
    pub hdr_supported: bool,
    pub hdr_enabled: bool,
    pub primary: bool,
    #[serde(skip)]
    pub adapter: LUID,
    #[serde(skip)]
    pub target: u32,
}
pub struct Topology {
    paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}
impl Topology {
    pub fn nodes(&self) -> Result<Vec<butterpollo_core::topology::Node>> {
        use butterpollo_core::topology::{Kind, Mode, Node, Position};
        self.monitors()
            .into_iter()
            .map(|m| {
                let mode = mode(&m.display_name)?;
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
                        refresh_hz: f64::from(mode.dmDisplayFrequency),
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
                    source.Anonymous.sourceMode.position = POINTL {
                        x: position.x,
                        y: position.y,
                    };
                }
            }
        }
        self.restore()
    }
    pub fn query() -> Result<Self> {
        unsafe {
            for _ in 0..8 {
                let (mut np, mut nm) = (0, 0);
                GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm).ok()?;
                if np > 256 || nm > 768 {
                    bail!("display configuration exceeds the limit");
                }
                let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
                let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
                let result = QueryDisplayConfig(
                    QDC_ONLY_ACTIVE_PATHS,
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
    pub fn monitors(&self) -> Vec<Monitor> {
        let primary = crate::capture::displays()
            .unwrap_or_default()
            .into_iter()
            .find(|d| d.primary)
            .map(|d| d.display_name);
        self.paths
            .iter()
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
                let mut color = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
                    header: header(
                        DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                        size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(),
                        p.targetInfo.adapterId,
                        p.targetInfo.id,
                    ),
                    ..Default::default()
                };
                let flags = if DisplayConfigGetDeviceInfo(&mut color.header) == 0 {
                    color.Anonymous.value
                } else {
                    0
                };
                let display_name = wide(&source.viewGdiDeviceName);
                Some(Monitor {
                    device_id: wide(&target.monitorDevicePath),
                    friendly_name: wide(&target.monitorFriendlyDeviceName),
                    primary: primary.as_ref() == Some(&display_name),
                    display_name,
                    hdr_supported: flags & 1 != 0,
                    hdr_enabled: flags & 2 != 0,
                    adapter: p.targetInfo.adapterId,
                    target: p.targetInfo.id,
                })
            })
            .collect()
    }
}
pub fn monitors() -> Result<Vec<Monitor>> {
    Ok(Topology::query()?.monitors())
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub nodes: Vec<butterpollo_core::topology::Node>,
    pub hdr: std::collections::BTreeMap<String, bool>,
}
impl Snapshot {
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
        })
    }
    pub fn restore(&self) -> Result<()> {
        if self.version != 1 {
            bail!("unsupported Rust display snapshot version");
        }
        let monitors = monitors()?;
        for n in &self.nodes {
            if let Some(m) = monitors.iter().find(|m| m.device_id == n.device_id) {
                set_mode(
                    &m.display_name,
                    n.mode.width,
                    n.mode.height,
                    n.mode.refresh_hz.round() as u32,
                )?;
                if let Some(enabled) = self.hdr.get(&n.device_id) {
                    set_hdr(m, *enabled)?;
                }
            }
        }
        Topology::query()?.set_positions(
            &self
                .nodes
                .iter()
                .map(|n| (n.device_id.clone(), n.desired_position))
                .collect(),
        )
    }
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
    unsafe {
        if enabled && !m.hdr_supported {
            bail!("selected display does not support HDR");
        }
        let color = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
                size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>(),
                m.adapter,
                m.target,
            ),
            Anonymous: DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE_0 {
                value: u32::from(enabled),
            },
        };
        check(DisplayConfigSetDeviceInfo(&color.header))
    }
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
    unsafe {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let result = ChangeDisplaySettingsExW(
            PCWSTR(name.as_ptr()),
            Some(mode),
            None,
            CDS_FULLSCREEN,
            None,
        );
        if result != DISP_CHANGE_SUCCESSFUL {
            bail!("Windows refused the requested display mode ({})", result.0);
        }
        Ok(())
    }
}
const NAMESPACE: [u8; 16] = [
    0x84, 0x42, 0x86, 0xa2, 0xfe, 0x77, 0x36, 0x43, 0xa8, 0x28, 0, 0xfe, 0xec, 0x89, 0xeb, 0xac,
];
struct Driver(HANDLE);
impl Driver {
    fn open() -> Result<Self> {
        let driver = Self(crate::input::open_interface(GUID::from_u128(
            0x5f894d6c_3a69_48a2_86ef_e4c671932d63,
        ))?);
        let version = driver.ioctl(0x900, 0, &[], 24)?;
        if version.len() != 24
            || version[..16] != NAMESPACE
            || u16::from_le_bytes(version[16..18].try_into().unwrap()) != 3
            || u16::from_le_bytes(version[18..20].try_into().unwrap()) < 6
        {
            bail!("incompatible virtual display driver protocol");
        }
        Ok(driver)
    }
    fn ioctl(&self, function: u32, access: u32, input: &[u8], size: usize) -> Result<Vec<u8>> {
        unsafe {
            let mut output = vec![0; size];
            let mut bytes = 0;
            DeviceIoControl(
                self.0,
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
            let _ = CloseHandle(self.0);
        }
    }
}
pub fn virtual_display_available() -> bool {
    Driver::open().is_ok()
}
pub struct VirtualDisplay {
    driver: Driver,
    pub name: String,
    lease: u64,
    id: u64,
    last_feed: Instant,
    mode: (u32, u32, u32),
}
// Driver IOCTLs use a thread-safe Windows device handle; shared access is
// serialized by the enclosing mutex, including feed and final teardown.
unsafe impl Send for VirtualDisplay {}
type DisplayLease = std::sync::Arc<std::sync::Mutex<VirtualDisplay>>;
static DISPLAYS: std::sync::Mutex<
    std::collections::BTreeMap<String, std::sync::Weak<std::sync::Mutex<VirtualDisplay>>>,
> = std::sync::Mutex::new(std::collections::BTreeMap::new());
fn display_lease(id: &str, width: u32, height: u32, fps: u32) -> Result<DisplayLease> {
    let mut displays = DISPLAYS.lock().unwrap();
    displays.retain(|_, lease| lease.strong_count() != 0);
    if let Some(display) = displays.get(id).and_then(std::sync::Weak::upgrade) {
        if display.lock().unwrap().mode != (width, height, fps) {
            bail!("shared virtual display already uses a different mode");
        }
        return Ok(display);
    }
    let display = std::sync::Arc::new(std::sync::Mutex::new(VirtualDisplay::create(
        id, width, height, fps,
    )?));
    displays.insert(id.into(), std::sync::Arc::downgrade(&display));
    Ok(display)
}
impl VirtualDisplay {
    pub fn create(stable_id: &str, width: u32, height: u32, fps: u32) -> Result<Self> {
        if !(320..=7680).contains(&width) || !(200..=4320).contains(&height) || fps == 0 {
            bail!("virtual display mode is outside the driver limits");
        }
        let driver = Driver::open()?;
        let lease = (u64::from_le_bytes(butterpollo_core::crypto::random())
            & 0x1fff_ffff_ffff_ffff)
            | 0x6000_0000_0000_0000;
        let mut id = 0xcbf29ce484222325u64;
        for b in stable_id.bytes() {
            id = (id ^ u64::from(b)).wrapping_mul(0x100000001b3);
        }
        let mut request = NAMESPACE.to_vec();
        request.extend_from_slice(&lease.to_le_bytes());
        request.extend_from_slice(&id.to_le_bytes());
        for value in [
            width,
            height,
            600,
            340,
            fps.checked_mul(1000).context("refresh overflow")?,
            10000,
        ] {
            request.extend_from_slice(&value.to_le_bytes());
        }
        let mut label = [0u8; 32];
        label[..15].copy_from_slice(b"Butterpollo Rust");
        request.extend_from_slice(&label);
        request.extend_from_slice(&1u32.to_le_bytes());
        request.extend_from_slice(&1000u32.to_le_bytes());
        request.extend_from_slice(&butterpollo_core::crypto::random::<32>());
        let result = driver.ioctl(0x90c, 3, &request, 56)?;
        let mut display = Self {
            driver,
            name: String::new(),
            lease,
            id,
            last_feed: Instant::now(),
            mode: (width, height, fps),
        };
        if result.len() != 56
            || result[..16] != NAMESPACE
            || result[16..24] != lease.to_le_bytes()
            || result[24..32] != id.to_le_bytes()
        {
            bail!("invalid virtual display identity response");
        }
        let luid = LUID {
            LowPart: u32::from_le_bytes(result[32..36].try_into().unwrap()),
            HighPart: i32::from_le_bytes(result[36..40].try_into().unwrap()),
        };
        let target = u32::from_le_bytes(result[40..44].try_into().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(monitors) = monitors()
                && let Some(m) = monitors
                    .into_iter()
                    .find(|m| m.adapter == luid && m.target == target)
            {
                display.name = m.display_name;
                return Ok(display);
            }
            display.feed()?;
            std::thread::sleep(Duration::from_millis(50));
        }
        bail!("virtual display did not become active before the deadline")
    }
    pub fn feed(&mut self) -> Result<()> {
        if self.last_feed.elapsed() >= Duration::from_secs(1) {
            let mut request = NAMESPACE.to_vec();
            request.extend_from_slice(&self.lease.to_le_bytes());
            request.extend_from_slice(&10000u32.to_le_bytes());
            request.extend_from_slice(&0u32.to_le_bytes());
            self.driver.ioctl(0x903, 3, &request, 0)?;
            self.last_feed = Instant::now();
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
    mode: Option<(DEVMODEW, (u32, u32, u32))>,
    color: Option<(Monitor, bool)>,
}
static SETTINGS: std::sync::Mutex<std::collections::BTreeMap<String, Settings>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());
pub struct Guard {
    pub output: String,
    virtual_display: Option<DisplayLease>,
    identity: String,
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
        change_mode: bool,
    ) -> Result<Self> {
        let virtual_display = if virtual_mode {
            Some(display_lease(stable_id, width, height, fps)?)
        } else {
            None
        };
        let choices = monitors()?;
        let chosen = if let Some(v) = &virtual_display {
            let name = v.lock().unwrap().name.clone();
            choices.iter().find(|d| d.display_name == name)
        } else {
            choices
                .iter()
                .find(|d| d.display_name == output || d.device_id == output)
                .or_else(|| choices.iter().find(|d| d.primary))
                .or_else(|| choices.first())
        }
        .context("display unavailable")?
        .clone();
        let guard = Self {
            output: chosen.display_name.clone(),
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
            if change_mode && guard.virtual_display.is_none() {
                let requested = (width, height, fps);
                if let Some((_, applied)) = &settings.mode {
                    if *applied != requested {
                        bail!("another stream owns a different display mode");
                    }
                } else {
                    let previous = mode(&guard.output)?;
                    if (
                        previous.dmPelsWidth,
                        previous.dmPelsHeight,
                        previous.dmDisplayFrequency,
                    ) != requested
                    {
                        crate::display_recovery::mode(
                            &guard.identity,
                            &guard.output,
                            (
                                previous.dmPelsWidth,
                                previous.dmPelsHeight,
                                previous.dmDisplayFrequency,
                            ),
                            requested,
                        )?;
                        settings.mode = Some((previous, requested));
                        set_mode(&guard.output, width, height, fps)?;
                    }
                }
            }
            if hdr && !chosen.hdr_enabled && settings.color.is_none() {
                crate::display_recovery::hdr(&guard.identity, &guard.output, false, true)?;
                settings.color = Some((chosen.clone(), false));
                set_hdr(&chosen, true)?;
            }
            Ok(())
        })();
        result?;
        Ok(guard)
    }
    pub fn feed(&mut self) -> Result<()> {
        if let Some(display) = &mut self.virtual_display {
            display.lock().unwrap().feed()?;
        }
        Ok(())
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
        let mut restored = true;
        if let Some((monitor, previous)) = settings.color
            && monitors().is_ok_and(|all| {
                all.iter()
                    .any(|m| m.device_id == monitor.device_id && m.hdr_enabled)
            })
            && let Err(e) = set_hdr(&monitor, previous)
        {
            restored = false;
            tracing::warn!(error=%e,"HDR restoration failed");
        }
        if let Some((previous, applied)) = settings.mode
            && mode(&self.output)
                .is_ok_and(|m| (m.dmPelsWidth, m.dmPelsHeight, m.dmDisplayFrequency) == applied)
            && let Err(e) = apply_mode(&self.output, &previous)
        {
            restored = false;
            tracing::warn!(error=%e,"display mode restoration failed");
        }
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
    pub fn create(id: &str, width: u32, height: u32, fps: u32, hdr: bool) -> Result<Self> {
        let guard = Guard::new(
            "",
            true,
            &format!("{id}:remote-monitor"),
            width,
            height,
            fps,
            hdr,
            false,
        )?;
        let output = guard.output.clone();
        let guard = std::sync::Arc::new(std::sync::Mutex::new(Some(guard)));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_guard = guard.clone();
        let worker_stop = stop.clone();
        let worker = std::thread::Builder::new()
            .name("monitor-lease".into())
            .spawn(move || {
                while !worker_stop.load(std::sync::atomic::Ordering::Acquire) {
                    if let Some(guard) = worker_guard.lock().unwrap().as_mut()
                        && let Err(e) = guard.feed()
                    {
                        tracing::warn!(error=%e,"retained monitor lease heartbeat failed");
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            })?;
        Ok(Self {
            output,
            mode: (width, height, fps, hdr),
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
