//! WGC in the signed-in user's process, with bounded GPU-only frame transport.
//!
//! SYSTEM cannot activate the per-user capture broker (CreateForMonitor returns
//! 0x80070424). Only capture runs in the user process; the host keeps its service
//! identity for input, display recovery and credentials. A private local pipe
//! carries metadata. Four unnamed keyed textures carry pixels, never the CPU.

use super::*;
use crate::ipc::Pipe;
use anyhow::ensure;
use butterpollo_core::config::Config;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::{Arc, Mutex},
};
use windows::Win32::{
    Foundation::*,
    System::{StationsAndDesktops::*, Threading::GetCurrentProcessId},
};
use windows::core::HRESULT;

const VERSION: u32 = 3;
const SLOTS: usize = 4;
const START_TIMEOUT: Duration = Duration::from_secs(5);
const PIPE_PREFIX: &str = r"\\.\pipe\Butterpollo.Wgc.";

fn owned(handle: HANDLE) -> OwnedHandle {
    // SAFETY: Callers pass a handle this process owns and closes nowhere else: a fresh
    // CreateSharedHandle result, or the event the pid-checked parent duplicated into this process.
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}
fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Request {
    Start {
        version: u32,
        name: String,
        hdr: bool,
        config: Config,
        /// An event of the host's, duplicated into the helper, set after
        /// each frame it announces.
        wake: u64,
    },
    Release {
        slot: usize,
    },
    Ping,
    Stop,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Reply {
    Alive,
    Ready {
        version: u32,
        handles: [u64; SLOTS],
        width: u32,
        height: u32,
        format: i32,
        compute: bool,
    },
    Frame {
        slot: usize,
        sequence: u64,
        qpc: i64,
    },
    Error {
        message: String,
        #[serde(default)]
        device_removed: Option<i32>,
    },
    ComputeFallback {
        message: String,
    },
}

/// No desktop mutation. A user capture cannot show Winlogon/UAC; the SYSTEM
/// host must temporarily use Desktop Duplication there.
pub(super) fn desktop_available() -> bool {
    // SAFETY: `desktop` is closed exactly once below, and `name` is a live buffer whose byte size
    // is passed.
    unsafe {
        let Ok(desktop) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS)
        else {
            return false;
        };
        let mut name = [0u16; 256];
        let result = GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            std::mem::size_of_val(&name) as u32,
            None,
        );
        let _ = CloseDesktop(desktop);
        result.is_ok() && crate::text::from_wide(&name).eq_ignore_ascii_case("default")
    }
}

/// AcquireSync's WAIT_TIMEOUT and WAIT_ABANDONED are *successful HRESULTs*.
/// The generated Result binding loses that distinction, so inspect the raw code.
fn acquire(mutex: &IDXGIKeyedMutex, key: u64) -> Result<bool> {
    // SAFETY: `mutex` is a live IDXGIKeyedMutex and AcquireSync takes (this, key, timeout) as
    // called here.
    let status = unsafe { (mutex.vtable().AcquireSync)(mutex.as_raw(), key, 0) };
    acquired_status(status)
}
fn acquired_status(status: HRESULT) -> Result<bool> {
    if status.0 == WAIT_TIMEOUT.0 as i32 {
        return Ok(false);
    }
    ensure!(
        status == HRESULT(0),
        "WGC shared texture mutex failed: {status:?}"
    );
    Ok(true)
}

fn capture_config(config: &Config) -> Config {
    const KEYS: &[&str] = &[
        "adapter_name",
        "adapter_pnp_id",
        "gpu_compute_conversion",
        "wgc_compute_copy",
        "wgc_high_rate_capture",
        "wgc_drain_to_newest",
        "wgc_helper_streaming_scope",
        "nvenc_realtime_hags",
    ];
    Config {
        values: config
            .values
            .iter()
            .filter(|(key, _)| KEYS.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    }
}
fn handoff(
    gpu: &Device,
    config: &Config,
    warnings: &butterpollo_core::session::Warnings,
) -> Option<crate::compute::Handoff> {
    if !config.boolean("wgc_compute_copy", true)
        || !crate::compute::enabled(config)
        || !crate::compute::copies_on(&gpu.device)
    {
        return None;
    }
    match crate::compute::Compute::for_device(&gpu.device)
        .and_then(|compute| crate::compute::Handoff::new(compute, gpu))
    {
        Ok(copy) => Some(copy),
        Err(error) => {
            warnings.set("capture_compute", format!("WGC helper compute copies unavailable ({error:#}); using the graphics queue. Lower game GPU load or update the AMD driver if capture stutters."));
            None
        }
    }
}

#[derive(Default)]
struct Slots {
    busy: [bool; SLOTS],
    sequence: u64,
}
impl Slots {
    fn publish(&mut self, slot: usize, sequence: u64) -> Result<()> {
        ensure!(
            slot < SLOTS && !self.busy[slot],
            "invalid or occupied WGC slot"
        );
        ensure!(sequence > self.sequence, "out-of-order WGC frame");
        self.busy[slot] = true;
        self.sequence = sequence;
        Ok(())
    }
    fn release(&mut self, slot: usize) -> Result<()> {
        ensure!(
            slot < SLOTS && self.busy[slot],
            "invalid or duplicate WGC release"
        );
        self.busy[slot] = false;
        Ok(())
    }
}

struct Texture {
    texture: ID3D11Texture2D,
    mutex: IDXGIKeyedMutex,
}

/// A helper texture that an image is read from in place (on AMD with
/// compute copies, unless `wgc_helper_zero_copy` is false) instead of being
/// copied into a host texture first. The host keeps its keyed mutex until the last clone of the image
/// is dropped, then hands it back once the compute work that read it is
/// done; the capture worker tells the helper on its next poll.
pub(crate) struct Lease {
    slot: usize,
    mutex: IDXGIKeyedMutex,
    returns: Arc<Mutex<crate::compute::Returns>>,
    freed: Arc<Mutex<Vec<usize>>>,
    wake: Arc<crate::timing::Signal>,
}
// SAFETY: The keyed mutex is used only in Drop, under `returns`' lock on the multithread-protected
// device; the rest is locks and an event handle.
unsafe impl Send for Lease {}
// SAFETY: Shared references never use the fields; only Drop, with exclusive access, does.
unsafe impl Sync for Lease {}
impl Drop for Lease {
    fn drop(&mut self) {
        let returned = self.returns.lock().unwrap().hand_back(&self.mutex, 0);
        if let Err(error) = returned {
            tracing::debug!(error = %format!("{error:#}"), slot = self.slot, "WGC helper texture handed back after a GPU error");
        }
        self.freed.lock().unwrap().push(self.slot);
        // Wake the capture worker so the helper hears of the free texture now.
        let _ = self.wake.set();
    }
}
fn validate_texture(
    desc: &D3D11_TEXTURE2D_DESC,
    width: u32,
    height: u32,
    format: i32,
) -> Result<()> {
    ensure!(
        width > 0 && height > 0 && width <= 16384 && height <= 16384,
        "invalid WGC texture dimensions"
    );
    ensure!(
        matches!(
            DXGI_FORMAT(format),
            DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_R16G16B16A16_FLOAT
        ),
        "unsupported WGC shared format"
    );
    ensure!(
        desc.Width == width
            && desc.Height == height
            && desc.Format.0 == format
            && desc.MipLevels == 1
            && desc.ArraySize == 1
            && desc.SampleDesc.Count == 1
            && desc.Usage == D3D11_USAGE_DEFAULT,
        "WGC shared texture descriptor mismatch"
    );
    let sharing =
        (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0) as u32;
    ensure!(
        desc.MiscFlags & sharing == sharing,
        "WGC texture is not synchronized for sharing"
    );
    Ok(())
}

pub struct Session {
    pipe: Pipe,
    pub(super) gpu: Device,
    textures: Vec<Texture>,
    owned: GpuPool,
    slots: Slots,
    pending: VecDeque<(usize, u64, Instant, Option<Instant>)>,
    published: u64,
    held: Option<(GpuImage, Instant)>,
    grid: Option<Arc<Mutex<ClaimGrid>>>,
    intervals: butterpollo_core::capture_policy::Intervals,
    origin: Instant,
    last_publish: Instant,
    desktop_check: Instant,
    heartbeat: Instant,
    last_reply: Instant,
    staging: Option<ID3D11Texture2D>,
    process: crate::process::Process,
    /// Set by the helper after each frame: the capture worker wakes for it
    /// instead of finding it on its next poll.
    frame: Arc<crate::timing::Signal>,
    /// Images read in place from the helper's textures, with the slots
    /// their leases gave back since the last poll.
    returns: Option<Arc<Mutex<crate::compute::Returns>>>,
    freed: Arc<Mutex<Vec<usize>>>,
}
impl Session {
    pub(super) fn new(
        name: &str,
        hdr: bool,
        config: &Config,
        warnings: Arc<butterpollo_core::session::Warnings>,
    ) -> Result<Self> {
        ensure!(
            desktop_available(),
            "WGC requires the unlocked user desktop"
        );
        let gpu = Device::for_display(
            name,
            config.get("adapter_name", ""),
            config.get("adapter_pnp_id", ""),
        )?;
        let _ = crate::gpu_priority::configure(&gpu, config);
        let (pipe, pipe_name) = Pipe::server(PIPE_PREFIX)?;
        let program = std::env::current_exe()?;
        let args = [
            "--wgc-worker".into(),
            pipe_name.into(),
            "--wgc-parent".into(),
            // SAFETY: GetCurrentProcessId has no preconditions.
            unsafe { GetCurrentProcessId() }.to_string().into(),
        ];
        let process = crate::process::Process::spawn(
            &program,
            &args,
            program.parent(),
            crate::process::Target::User { elevated: false },
            &BTreeMap::new(),
            true,
        )
        .context("start WGC in the signed-in user's session")?;
        let deadline = Instant::now() + START_TIMEOUT;
        while !pipe.connected(process.pid)? {
            ensure!(
                process.exit_code()?.is_none(),
                "WGC helper exited before connecting"
            );
            ensure!(Instant::now() < deadline, "WGC helper connection timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
        let frame = Arc::new(crate::timing::Signal::new()?);
        pipe.send(&Request::Start {
            version: VERSION,
            name: gpu.display.display_name.clone(),
            hdr,
            config: capture_config(config),
            wake: process.duplicate_into(frame.raw())?,
        })?;
        let (handles, width, height, format, worker_compute) = loop {
            match pipe.receive::<Reply>()? {
                Some(Reply::Ready {
                    version,
                    handles,
                    width,
                    height,
                    format,
                    compute,
                }) => {
                    ensure!(version == VERSION, "WGC helper version mismatch");
                    break (handles, width, height, format, compute);
                }
                Some(Reply::Error {
                    message,
                    device_removed,
                }) => return Err(helper_error(message, device_removed)),
                Some(_) => bail!("WGC helper sent a frame before its textures"),
                None => {}
            }
            ensure!(
                process.exit_code()?.is_none(),
                "WGC helper exited during startup"
            );
            ensure!(
                Instant::now() < deadline,
                "WGC helper did not produce a first frame"
            );
            std::thread::sleep(Duration::from_millis(2));
        };
        let device: ID3D11Device1 = gpu.device.cast()?;
        let mut owned = GpuPool {
            compute: handoff(&gpu, config, &warnings),
            warnings: warnings.clone(),
            ..Default::default()
        };
        // Reading the helper's textures in place skips the host's copy. Only
        // compute work and this device's context may read them: the lease
        // waits for those before the helper may write again.
        let mut returns = (config.boolean("wgc_helper_zero_copy", true)
            && owned.compute.is_some())
        .then(|| {
            crate::compute::Compute::for_device(&gpu.device)
                .and_then(|compute| crate::compute::Returns::new(compute, &gpu))
        })
        .transpose()
        .unwrap_or_else(|error| {
            tracing::warn!(error = %format!("{error:#}"), "WGC helper textures cannot be read in place; copying them");
            None
        });
        let mut textures = Vec::with_capacity(SLOTS);
        for handle in handles {
            let handle = process.duplicate_resource(usize::try_from(handle)?)?;
            // SAFETY: `handle` is the helper's shared texture duplicated into this process; `desc`
            // is checked below.
            let texture: ID3D11Texture2D = unsafe { device.OpenSharedResource1(raw(&handle))? };
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: `texture` is a live texture and `desc` a live local.
            unsafe {
                texture.GetDesc(&mut desc);
            }
            validate_texture(&desc, width, height, format)?;
            if let Some(copy) = &mut owned.compute
                && let Err(error) = copy.import_shared(&texture, raw(&handle))
            {
                warnings.set("capture_compute", format!("WGC shared texture cannot use compute ({error:#}); using the graphics queue. Lower game GPU load or update the AMD driver if capture stutters."));
                owned.compute = None;
                returns = None;
            }
            if let Some(lender) = &mut returns
                && let Err(error) = lender.import(&texture, raw(&handle))
            {
                tracing::warn!(error = %format!("{error:#}"), "WGC helper textures cannot be read in place; copying them");
                returns = None;
            }
            textures.push(Texture {
                mutex: texture.cast()?,
                texture,
            });
        }
        tracing::info!(
            pid = process.pid,
            width,
            height,
            compute = owned.compute.is_some(),
            worker_compute,
            zero_copy = returns.is_some(),
            "Windows Graphics Capture running in the signed-in user's process"
        );
        let now = Instant::now();
        Ok(Self {
            pipe,
            gpu,
            textures,
            owned,
            slots: Slots::default(),
            pending: VecDeque::with_capacity(SLOTS),
            published: 0,
            held: None,
            grid: None,
            intervals: Default::default(),
            origin: now,
            last_publish: now,
            desktop_check: now,
            heartbeat: now + Duration::from_secs(1),
            last_reply: now,
            staging: None,
            process,
            frame,
            returns: returns.map(|returns| Arc::new(Mutex::new(returns))),
            freed: Arc::default(),
        })
    }
    pub(super) fn frame_signal(&self) -> &crate::timing::Signal {
        &self.frame
    }
    pub(super) fn set_claim_grid(&mut self, grid: Option<Arc<Mutex<ClaimGrid>>>) {
        self.grid = grid;
    }
    pub(super) fn publication_deadline(&self) -> Option<Instant> {
        self.held.as_ref().map(|(_, deadline)| *deadline)
    }
    pub(super) fn next_frame(&mut self) -> Result<Option<Image>> {
        self.next_gpu()?
            .map(|image| image.readback(&mut self.staging))
            .transpose()
    }
    pub(super) fn next_gpu(&mut self) -> Result<Option<GpuImage>> {
        let now = Instant::now();
        if now >= self.desktop_check {
            self.desktop_check = now + Duration::from_millis(100);
            ensure!(
                desktop_available(),
                "WGC user desktop changed; Desktop Duplication required"
            );
            ensure!(self.process.exit_code()?.is_none(), "WGC helper exited");
        }
        if now >= self.heartbeat {
            self.pipe.send(&Request::Ping)?;
            self.heartbeat = now + Duration::from_secs(1);
        }
        let freed = std::mem::take(&mut *self.freed.lock().unwrap());
        for slot in freed {
            self.slots.release(slot)?;
            self.pipe.send(&Request::Release { slot })?;
        }
        // At most three frames can be outstanding. Never accumulate a queue
        // behind an encoder; copy only the newest frame whose GPU work is done.
        for _ in 0..=SLOTS {
            match self.pipe.receive::<Reply>()? {
                Some(Reply::Frame {
                    slot,
                    sequence,
                    qpc,
                }) => {
                    self.slots.publish(slot, sequence)?;
                    self.last_reply = now;
                    let (captured, stamp) = qpc_timestamps(qpc);
                    self.pending.push_back((slot, sequence, captured, stamp));
                }
                Some(Reply::Alive) => self.last_reply = now,
                Some(Reply::ComputeFallback { message }) => {
                    self.last_reply = now;
                    self.owned.warnings.set("capture_helper_compute", format!("WGC helper compute copy failed ({message}); transfer uses the graphics queue. Lower game GPU load or update the AMD driver if capture stutters."));
                }
                Some(Reply::Error {
                    message,
                    device_removed,
                }) => return Err(helper_error(message, device_removed)),
                Some(Reply::Ready { .. }) => bail!("WGC helper replaced live textures"),
                None => break,
            }
        }
        ensure!(
            now.duration_since(self.last_reply) < START_TIMEOUT,
            "WGC helper stopped responding"
        );
        let mut newest = None;
        let mut attempted_copy = false;
        for index in (0..self.pending.len()).rev() {
            let (slot, sequence, captured, stamp) = self.pending[index];
            let texture = &self.textures[slot];
            if !acquire(&texture.mutex, 1)? {
                continue;
            }
            let copied = if !attempted_copy && sequence > self.published {
                attempted_copy = true;
                match &self.returns {
                    Some(returns) => self.lend(slot, returns.clone()).map(Some),
                    None => self.owned.copy(&self.gpu, &texture.texture),
                }
            } else {
                Ok(None)
            };
            self.pending.remove(index);
            // A lent texture goes back when its image is dropped.
            if !matches!(&copied, Ok(Some(image)) if image.lease.is_some()) {
                // Handoff orders the context after its compute copy. Releasing the
                // keyed mutex therefore cannot let the helper overwrite a texture
                // while our GPU is still reading it, even without a CPU wait.
                // SAFETY: This process acquired the keyed mutex with key 1 above, and the context is
                // this device's.
                unsafe {
                    texture.mutex.ReleaseSync(0)?;
                    self.gpu.context.Flush();
                }
                self.slots.release(slot)?;
                self.pipe.send(&Request::Release { slot })?;
            }
            if let Some(mut image) = copied? {
                image.captured = captured;
                image.wgc_stamp = stamp;
                self.published = sequence;
                newest = Some(image);
            }
        }
        if let Some(image) = newest {
            let composition = self.intervals.observe(nanos(now, self.origin));
            let deadline = self
                .grid
                .as_ref()
                .and_then(|grid| {
                    let grid = grid.lock().unwrap();
                    butterpollo_core::capture_policy::publication_deadline(
                        grid.period.as_nanos().min(i64::MAX as u128) as i64,
                        nanos(now, grid.anchor),
                        composition,
                        nanos(self.last_publish, grid.anchor),
                    )
                    .and_then(|deadline| {
                        if deadline >= 0 {
                            grid.anchor
                                .checked_add(Duration::from_nanos(deadline as u64))
                        } else {
                            grid.anchor
                                .checked_sub(Duration::from_nanos(deadline.unsigned_abs()))
                        }
                    })
                })
                .unwrap_or(now);
            self.held = Some((image, deadline));
        }
        if self
            .held
            .as_ref()
            .is_some_and(|(_, deadline)| *deadline <= now)
        {
            self.last_publish = now;
            return Ok(self.held.take().map(|(image, _)| image));
        }
        Ok(None)
    }
}
impl Session {
    /// An image of helper texture `slot`, read in place; this process holds
    /// its keyed mutex (key 1) until the image's lease hands it back.
    fn lend(&self, slot: usize, returns: Arc<Mutex<crate::compute::Returns>>) -> Result<GpuImage> {
        let texture = &self.textures[slot];
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `texture` is a live texture and `desc` a live local.
        unsafe {
            texture.texture.GetDesc(&mut desc);
        }
        let pixel = match desc.Format {
            DXGI_FORMAT_B8G8R8A8_UNORM => Pixel::Bgra8,
            DXGI_FORMAT_R16G16B16A16_FLOAT => Pixel::RgbaF16,
            _ => bail!("unsupported WGC shared format {:?}", desc.Format),
        };
        let ready = returns.lock().unwrap().acquired()?;
        Ok(GpuImage {
            cursor: None,
            width: desc.Width,
            height: desc.Height,
            pixel,
            captured: Instant::now(),
            wgc_stamp: None,
            acquired: Instant::now(),
            gpu: self.gpu.clone(),
            texture: Arc::new(texture.texture.clone()),
            ready: Some(ready),
            lease: Some(Arc::new(Lease {
                slot,
                mutex: texture.mutex.clone(),
                returns,
                freed: self.freed.clone(),
                wake: self.frame.clone(),
            })),
        })
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.pipe.send(&Request::Stop);
        // The owned job also kills the helper if it crashes, hangs in WGC, or
        // outlives this session. No helper survives capture/device recovery.
    }
}

struct NativeFrame(Direct3D11CaptureFrame);
impl Drop for NativeFrame {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}
fn next_native(capture: &mut Wgc) -> Result<Option<NativeFrame>> {
    capture.check_color_space()?;
    capture
        .next_native_frame()
        .map(|frame| frame.map(NativeFrame))
        .map_err(
            |error| match crate::device_loss::DeviceLost::d3d11(&capture.gpu.device) {
                Some(loss) => error.context(loss),
                None => error,
            },
        )
}
fn helper_error(message: String, reason: Option<i32>) -> anyhow::Error {
    let error = anyhow::anyhow!("WGC helper: {message}");
    match reason {
        Some(reason) => error.context(crate::device_loss::DeviceLost(reason)),
        None => error,
    }
}
fn native_texture(frame: &NativeFrame) -> Result<ID3D11Texture2D> {
    let access: IDirect3DDxgiInterfaceAccess = frame.0.Surface()?.cast()?;
    // SAFETY: `access` is the surface of a frame that `frame` keeps open for this call.
    Ok(unsafe { access.GetInterface()? })
}

/// Internal entry point. It runs before the host opens credentials, listeners,
/// virtual displays or the tray, and never needs administrator privileges.
pub fn run_worker(pipe: &str, parent: u32) -> Result<()> {
    let pipe = Pipe::client(pipe, parent, PIPE_PREFIX)?;
    let result = worker(&pipe);
    if let Err(error) = &result {
        let _ = pipe.send(&Reply::Error {
            message: format!("{error:#}").chars().take(512).collect(),
            device_removed: crate::device_loss::DeviceLost::from_error(error).map(|loss| loss.0),
        });
        // Give the parent a chance to read the failure before closing its last
        // client handle; never wait indefinitely for a vanished host.
        std::thread::sleep(Duration::from_millis(20));
    }
    result
}
fn worker(pipe: &Pipe) -> Result<()> {
    ensure!(
        !crate::process::is_system(),
        "WGC helper must run as the signed-in user"
    );
    let deadline = Instant::now() + START_TIMEOUT;
    let (name, hdr, config, wake) = loop {
        match pipe.receive::<Request>()? {
            Some(Request::Start {
                version,
                name,
                hdr,
                config,
                wake,
            }) => {
                ensure!(version == VERSION, "unsupported WGC protocol version");
                // The host duplicated it into this process; closed on exit.
                let wake = owned(HANDLE(usize::try_from(wake)? as *mut _));
                break (name, hdr, capture_config(&config), wake);
            }
            Some(Request::Stop) => return Ok(()),
            Some(_) => bail!("unexpected WGC startup command"),
            None => {}
        }
        ensure!(
            Instant::now() < deadline,
            "WGC parent did not initialize capture"
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    let _com = ComGuard::new()?;
    let _priority = Priority::new();
    crate::timing::disable_power_throttling();
    let _streaming = config
        .boolean("wgc_helper_streaming_scope", false)
        .then(crate::timing::StreamingScope::enter);
    let timer = crate::timing::Timer::new()?;
    let gpu = Device::for_display(
        &name,
        config.get("adapter_name", ""),
        config.get("adapter_pnp_id", ""),
    )?;
    ensure!(
        gpu.display.display_name == name,
        "WGC output disappeared before helper startup"
    );
    let _ = crate::gpu_priority::configure(&gpu, &config);
    let mut capture = Wgc::new_device(
        gpu.clone(),
        hdr,
        config.boolean("wgc_high_rate_capture", false),
    )?;
    capture.drain_to_newest = config.boolean("wgc_drain_to_newest", false);
    // WGC delivers a frame when the desktop is composed, and a static desktop
    // may not be for seconds: the host then gave up on WGC for the whole
    // session (Desktop Duplication paces worse). A repaint of every window
    // composes one; nothing visible changes.
    let mut repaint = Instant::now() + Duration::from_millis(250);
    let mut first = loop {
        if let Some(frame) = next_native(&mut capture)? {
            break Some(frame);
        }
        if Instant::now() >= repaint {
            repaint = Instant::now() + Duration::from_millis(500);
            // SAFETY: RedrawWindow takes no pointers here and only queues repaints.
            unsafe {
                use windows::Win32::Graphics::Gdi::{
                    RDW_ALLCHILDREN, RDW_INVALIDATE, RedrawWindow,
                };
                let _ = RedrawWindow(None, None, None, RDW_INVALIDATE | RDW_ALLCHILDREN);
            }
        }
        if let Some(command) = pipe.receive::<Request>()? {
            ensure!(
                matches!(command, Request::Stop),
                "unexpected WGC command before the first frame"
            );
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Windows Graphics Capture produced no initial frame"
        );
        timer.until(Instant::now() + Duration::from_micros(500));
    };
    let source = native_texture(first.as_ref().unwrap())?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: `source` is a live texture and `desc` a live local.
    unsafe {
        source.GetDesc(&mut desc);
    }
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
    desc.CPUAccessFlags = 0;
    desc.MiscFlags =
        (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0) as u32;
    validate_texture(&desc, desc.Width, desc.Height, desc.Format.0)?;
    let compute_warnings = butterpollo_core::session::Warnings::default();
    let mut copy = handoff(&gpu, &config, &compute_warnings);
    let mut textures = Vec::with_capacity(SLOTS);
    let mut handles = Vec::with_capacity(SLOTS);
    for _ in 0..SLOTS {
        // SAFETY: `gpu.device` is live and `desc` was validated above; each shared handle is owned
        // by `handles`.
        unsafe {
            let mut texture = None;
            gpu.device
                .CreateTexture2D(&desc, None, Some(&mut texture))?;
            let texture = texture.context("WGC shared texture missing")?;
            let handle = owned(texture.cast::<IDXGIResource1>()?.CreateSharedHandle(
                None,
                DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                None,
            )?);
            if let Some(compute) = &mut copy
                && compute.import_shared(&texture, raw(&handle)).is_err()
            {
                copy = None;
            }
            textures.push(Texture {
                mutex: texture.cast()?,
                texture,
            });
            handles.push(handle);
        }
    }
    pipe.send(&Reply::Ready {
        version: VERSION,
        handles: std::array::from_fn(|index| raw(&handles[index]).0 as u64),
        width: desc.Width,
        height: desc.Height,
        format: desc.Format.0,
        compute: copy.is_some(),
    })?;
    for warning in compute_warnings.snapshot() {
        pipe.send(&Reply::ComputeFallback {
            message: warning.message,
        })?;
    }

    let mut slots = Slots::default();
    loop {
        for _ in 0..=SLOTS {
            match pipe.receive::<Request>()? {
                Some(Request::Stop) => return Ok(()),
                Some(Request::Release { slot }) => slots.release(slot)?,
                Some(Request::Ping) => pipe.send(&Reply::Alive)?,
                Some(Request::Start { .. }) => bail!("WGC cannot replace a live session"),
                None => break,
            }
        }
        let frame = if first.is_some() {
            first.take()
        } else {
            next_native(&mut capture)?
        };
        if let Some(frame) = frame {
            let mut chosen = None;
            for (slot, texture) in textures.iter().enumerate() {
                if !slots.busy[slot] && acquire(&texture.mutex, 0)? {
                    chosen = Some(slot);
                    break;
                }
            }
            if let Some(slot) = chosen {
                let texture = &textures[slot];
                let result = (|| -> Result<()> {
                    let source = native_texture(&frame)?;
                    if let Some(compute) = &mut copy
                        && let Err(error) = compute.copy(&texture.texture, &source)
                    {
                        if let Some(loss) = crate::device_loss::DeviceLost::d3d11(&gpu.device)
                            .or_else(|| compute.device_removed())
                            .or_else(|| crate::device_loss::DeviceLost::from_error(&error))
                        {
                            return Err(error.context(loss));
                        }
                        copy = None;
                        pipe.send(&Reply::ComputeFallback {
                            message: format!("{error:#}").chars().take(512).collect(),
                        })?;
                    }
                    if copy.is_none() {
                        // SAFETY: Both textures live on `gpu.device`, and this process holds
                        // `texture`'s keyed mutex (key 0).
                        unsafe {
                            gpu.context.CopyResource(&texture.texture, &source);
                        }
                    }
                    Ok(())
                })();
                // Release is queued after the compute handoff's context wait.
                // Thus the host sees complete pixels even under GPU load.
                // SAFETY: This process acquired the keyed mutex with key 0 above, and the context
                // is this device's.
                unsafe {
                    texture.mutex.ReleaseSync(1)?;
                    gpu.context.Flush();
                }
                result?;
                let sequence = slots
                    .sequence
                    .checked_add(1)
                    .context("WGC sequence exhausted")?;
                slots.publish(slot, sequence)?;
                let relative = frame.0.SystemRelativeTime()?.Duration;
                let qpc = wgc_qpc(relative, qpc_frequency());
                pipe.send(&Reply::Frame {
                    slot,
                    sequence,
                    qpc,
                })?;
                // SAFETY: `wake` is an event handle this process owns until `worker` returns.
                unsafe {
                    let _ = windows::Win32::System::Threading::SetEvent(raw(&wake));
                }
            }
            // A slow host drops updates. It never owns all of WGC's native
            // frame-pool entries or grows a queue of stale pictures.
        }
        timer.until(Instant::now() + Duration::from_micros(500));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn helper_reply_preserves_device_removal_and_accepts_older_errors() {
        let loss = crate::device_loss::DeviceLost(0x887a0006_u32 as i32);
        let reply = super::Reply::Error {
            message: "frame pool failed".into(),
            device_removed: Some(loss.0),
        };
        let json = serde_json::to_string(&reply).unwrap();
        let super::Reply::Error {
            message,
            device_removed,
        } = serde_json::from_str(&json).unwrap()
        else {
            panic!("wrong reply")
        };
        assert_eq!(
            crate::device_loss::DeviceLost::from_error(&super::helper_error(
                message,
                device_removed
            )),
            Some(loss)
        );
        let old = r#"{"type":"Error","message":"capture unavailable"}"#;
        let super::Reply::Error {
            message,
            device_removed,
        } = serde_json::from_str(old).unwrap()
        else {
            panic!("wrong reply")
        };
        assert!(
            crate::device_loss::DeviceLost::from_error(&super::helper_error(
                message,
                device_removed
            ))
            .is_none()
        );
    }
    use super::*;
    #[test]
    fn positive_mutex_wait_codes_never_grant_access_to_incomplete_pixels() {
        assert!(acquired_status(HRESULT(0)).unwrap());
        assert!(!acquired_status(HRESULT(WAIT_TIMEOUT.0 as i32)).unwrap());
        assert!(acquired_status(HRESULT(WAIT_ABANDONED.0 as i32)).is_err());
        assert!(acquired_status(HRESULT::from_win32(ERROR_ACCESS_DENIED.0)).is_err());
    }
    #[test]
    fn private_pipe_checks_peers_bounds_messages_and_reports_disconnect() -> Result<()> {
        let (server, name) = Pipe::server(PIPE_PREFIX)?;
        // SAFETY: GetCurrentProcessId has no preconditions.
        let pid = unsafe { GetCurrentProcessId() };
        assert!(Pipe::client(&name, pid.wrapping_add(1), PIPE_PREFIX).is_err());
        // The rejected client closes its endpoint; use a fresh single-instance
        // server, just as failed capture startup does.
        drop(server);
        let (server, name) = Pipe::server(PIPE_PREFIX)?;
        let client = Pipe::client(&name, pid, PIPE_PREFIX)?;
        assert!(server.connected(pid)?);
        assert!(server.connected(pid.wrapping_add(1)).is_err());
        assert!(server.receive::<Request>()?.is_none());
        client.send(&Request::Release { slot: 2 })?;
        assert!(matches!(
            server.receive::<Request>()?,
            Some(Request::Release { slot: 2 })
        ));
        assert!(server.receive::<Request>()?.is_none());
        server.send(&Reply::Frame {
            slot: 1,
            sequence: 15,
            qpc: 1234,
        })?;
        assert!(matches!(
            client.receive::<Reply>()?,
            Some(Reply::Frame {
                slot: 1,
                sequence: 15,
                qpc: 1234
            })
        ));
        assert!(client.send(&"x".repeat(crate::ipc::MESSAGE_LIMIT)).is_err());
        assert!(server.receive::<Request>()?.is_none());
        drop(client);
        assert!(server.receive::<Request>().is_err());
        Ok(())
    }
    #[test]
    fn slots_bound_outstanding_frames_and_reject_stale_or_duplicate_messages() {
        let mut slots = Slots::default();
        for slot in 0..SLOTS {
            slots.publish(slot, slot as u64 + 1).unwrap();
        }
        let next = SLOTS as u64 + 1;
        assert!(slots.publish(0, next).is_err());
        assert!(slots.publish(SLOTS, next).is_err());
        assert!(slots.release(SLOTS).is_err());
        slots.release(0).unwrap();
        assert!(slots.release(0).is_err());
        assert!(slots.publish(0, next - 1).is_err());
        slots.publish(0, next).unwrap();
        assert!(slots.busy.iter().all(|busy| *busy));
    }
    #[test]
    fn helper_receives_only_capture_settings_and_no_host_secrets_or_commands() {
        let source = Config::parse("adapter_name = AMD\ngpu_compute_conversion = false\nwgc_high_rate_capture = true\nwgc_drain_to_newest = true\nwgc_helper_streaming_scope = true\ncredentials_file = secret\nprep_cmd = run-something\nport = 47989\nwgc_user_helper = true\n").unwrap();
        let filtered = capture_config(&source);
        assert_eq!(filtered.values.len(), 5);
        assert_eq!(filtered.get("adapter_name", ""), "AMD");
        assert!(!filtered.boolean("gpu_compute_conversion", true));
        assert!(filtered.boolean("wgc_high_rate_capture", false));
        assert!(filtered.boolean("wgc_drain_to_newest", false));
        assert!(filtered.boolean("wgc_helper_streaming_scope", false));
        let message = Request::Start {
            version: VERSION,
            name: "display".into(),
            hdr: true,
            config: filtered,
            wake: 0x1234,
        };
        let encoded = serde_json::to_vec(&message).unwrap();
        assert!(encoded.len() < crate::ipc::MESSAGE_LIMIT);
        let decoded: Request = serde_json::from_slice(&encoded).unwrap();
        assert!(matches!(decoded, Request::Start { hdr: true, .. }));
        assert!(
            serde_json::from_str::<Request>(r#"{"type":"Release","slot":0,"command":"run"}"#)
                .is_err()
        );
    }
    #[test]
    fn shared_descriptors_must_match_the_supported_bounded_texture_contract() {
        let mut desc = D3D11_TEXTURE2D_DESC {
            Width: 1920,
            Height: 1080,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0
                | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0) as u32,
            ..Default::default()
        };
        assert!(validate_texture(&desc, 1920, 1080, desc.Format.0).is_ok());
        assert!(validate_texture(&desc, 16385, 1080, desc.Format.0).is_err());
        assert!(validate_texture(&desc, 1920, 1081, desc.Format.0).is_err());
        desc.MiscFlags = D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 as u32;
        assert!(validate_texture(&desc, 1920, 1080, desc.Format.0).is_err());
    }
}
