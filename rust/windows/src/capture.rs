use crate::text::from_wide;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::time::{Duration, Instant};
use windows::{
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
            GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
        System::{
            Com::*,
            WinRT::{
                Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
                Graphics::Capture::IGraphicsCaptureItemInterop,
            },
        },
    },
    core::{Interface, PCWSTR},
};

#[derive(Clone, Debug, Serialize)]
pub struct Display {
    pub device_id: String,
    pub display_name: String,
    pub friendly_name: String,
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub primary: bool,
    pub adapter: String,
    #[serde(skip)]
    pub adapter_index: u32,
    #[serde(skip)]
    pub output_index: u32,
}
pub fn enable_dpi_awareness() {
    // SAFETY: SetProcessDpiAwarenessContext takes a predefined context constant and no pointers.
    unsafe {
        // A failed call means the embedding process already set its DPI context.
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
}
pub fn displays() -> Result<Vec<Display>> {
    // SAFETY: The factory, adapters and outputs are live COM objects returned by these calls, and
    // each GetDesc returns an owned struct.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut result = vec![];
        for a in 0..32 {
            let Ok(adapter) = factory.EnumAdapters1(a) else {
                break;
            };
            let ad = adapter.GetDesc1()?;
            for o in 0..32 {
                let Ok(output) = adapter.EnumOutputs(o) else {
                    break;
                };
                let d = output.GetDesc()?;
                if !d.AttachedToDesktop.as_bool() {
                    continue;
                }
                let name = from_wide(&d.DeviceName);
                let r = d.DesktopCoordinates;
                result.push(Display {
                    device_id: name.clone(),
                    display_name: name,
                    friendly_name: from_wide(&ad.Description),
                    width: (r.right - r.left) as u32,
                    height: (r.bottom - r.top) as u32,
                    x: r.left,
                    y: r.top,
                    primary: r.left == 0 && r.top == 0,
                    adapter: from_wide(&ad.Description),
                    adapter_index: a,
                    output_index: o,
                });
            }
        }
        Ok(result)
    }
}
fn select_display<'a>(choices: &'a [Display], name: &str) -> Result<Option<&'a Display>> {
    if name.is_empty() {
        return Ok(choices
            .iter()
            .find(|d| d.primary)
            .or_else(|| choices.first()));
    }
    // A selected VDD can disappear during a mode change. Capture must wait
    // for it to return instead of reopening on the user's physical desktop.
    choices
        .iter()
        .find(|d| d.display_name == name || d.device_id == name)
        .map(Some)
        .with_context(|| format!("selected capture display is unavailable: {name}"))
}
/// Whether capture can open this display now. An empty name is the primary
/// display.
pub fn display_present(name: &str) -> bool {
    displays().is_ok_and(|choices| select_display(&choices, name).is_ok_and(|d| d.is_some()))
}
/// The primary display's name, which capture opens when the stream's display
/// did not come back.
pub fn primary_display() -> Option<String> {
    let choices = displays().ok()?;
    select_display(&choices, "")
        .ok()
        .flatten()
        .map(|d| d.display_name.clone())
}
pub struct ComGuard;
#[derive(serde::Serialize)]
pub struct Gpu {
    pub name: String,
    pub vendor: u32,
    pub dedicated_memory: u64,
    pub luid: (u32, i32),
    pub pnp_id: Option<String>,
}
fn adapter_pnp_id(luid: windows::Win32::Foundation::LUID) -> Option<String> {
    use windows::Win32::Devices::Display::*;
    let mut info = DISPLAYCONFIG_ADAPTER_NAME::default();
    info.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME;
    info.header.size = std::mem::size_of_val(&info) as u32;
    info.header.adapterId = luid;
    // SAFETY: `info.header` names the request type and the full size of `info`, so the call writes
    // only within `info`.
    if unsafe { DisplayConfigGetDeviceInfo(&mut info.header) } != 0 {
        return None;
    }
    let path = from_wide(&info.adapterDevicePath);
    let path = path.strip_prefix(r"\\?\")?;
    let mut parts = path.split('#');
    let (bus, hardware, instance) = (parts.next()?, parts.next()?, parts.next()?);
    if bus.is_empty() || hardware.is_empty() || instance.is_empty() {
        return None;
    }
    Some(format!("{bus}\\{hardware}\\{instance}"))
}
pub fn gpus() -> Result<Vec<Gpu>> {
    // SAFETY: The factory and adapters are live COM objects returned by these calls, and GetDesc1
    // returns an owned struct.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut result = Vec::new();
        for index in 0..32 {
            let Ok(adapter) = factory.EnumAdapters1(index) else {
                break;
            };
            let info = adapter.GetDesc1()?;
            if info.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 {
                result.push(Gpu {
                    name: from_wide(&info.Description),
                    vendor: info.VendorId,
                    dedicated_memory: info.DedicatedVideoMemory as u64,
                    luid: (info.AdapterLuid.LowPart, info.AdapterLuid.HighPart),
                    pnp_id: adapter_pnp_id(info.AdapterLuid),
                });
            }
        }
        Ok(result)
    }
}
impl ComGuard {
    pub fn new() -> Result<Self> {
        // SAFETY: CoInitializeEx takes no pointers here, and only a successful call yields the
        // guard whose Drop balances it.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: CoUninitialize takes no arguments, and `new` returns a guard only after
        // CoInitializeEx succeeded.
        unsafe {
            CoUninitialize();
        }
    }
}
pub struct Priority {
    handle: windows::Win32::Foundation::HANDLE,
}
impl Default for Priority {
    fn default() -> Self {
        Self::new()
    }
}

impl Priority {
    pub fn new() -> Self {
        let priority = Self::games();
        // SAFETY: SetThreadPriority acts on the current thread's pseudo-handle and takes no
        // pointers.
        unsafe {
            use windows::Win32::System::Threading::*;
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
        }
        priority
    }
    /// For the input thread: the MMCSS boost alone. The thread priority that
    /// new() sets afterwards cancels the boost (14 instead of 18), and input
    /// then waited milliseconds, not microseconds, behind busy time-critical
    /// threads. Without MMCSS the thread is raised as new() raises it.
    pub fn input() -> Self {
        let priority = Self::games();
        if priority.handle.is_invalid() {
            // SAFETY: SetThreadPriority acts on the current thread's pseudo-handle and takes no
            // pointers.
            unsafe {
                use windows::Win32::System::Threading::*;
                let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
            }
        }
        priority
    }
    /// Register the thread with MMCSS's "Games" task at high priority.
    fn games() -> Self {
        // SAFETY: `name` is a NUL-terminated UTF-16 buffer alive for the call, `index` is a live
        // local, and the handle is checked before use.
        unsafe {
            use windows::Win32::System::Threading::*;
            let name: Vec<u16> = "Games\0".encode_utf16().collect();
            let mut index = 0;
            let handle = windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW(
                PCWSTR(name.as_ptr()),
                &mut index,
            )
            .unwrap_or_default();
            if !handle.is_invalid() {
                let _ = AvSetMmThreadPriority(handle, AVRT_PRIORITY_HIGH);
            }
            Self { handle }
        }
    }
}
impl Drop for Priority {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: `handle` is the valid MMCSS handle `games` registered on this thread (HANDLE
            // keeps `Priority` on it), reverted only here.
            unsafe {
                let _ =
                    windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(self.handle);
            }
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pixel {
    Bgra8,
    RgbaF16,
    Rgba10Pq,
}
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub bytes: Vec<u8>,
    pub captured: Instant,
    pub pixel: Pixel,
}
#[derive(Clone)]
pub struct Device {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    /// The monitor the device was opened for; absent on a host without one,
    /// where the device can still encode.
    pub output: Option<IDXGIOutput1>,
    pub display: Display,
}
impl Device {
    /// The monitor this device captures.
    pub fn output(&self) -> Result<&IDXGIOutput1> {
        self.output
            .as_ref()
            .context("no monitor is attached to this GPU")
    }
    /// Read the selected output's luminance; conversion produces Rec.2020/D65.
    pub fn hdr_metadata(&self) -> butterpollo_core::hdr::Metadata {
        self.output
            .as_ref()
            // SAFETY: `output` is a live IDXGIOutput1, and GetDesc1 returns an owned struct.
            .and_then(|output| unsafe {
                output
                    .cast::<IDXGIOutput6>()
                    .and_then(|output| output.GetDesc1())
                    .ok()
            })
            .map(|desc| {
                butterpollo_core::hdr::Metadata::display(
                    desc.MaxLuminance,
                    desc.MinLuminance,
                    desc.MaxFullFrameLuminance,
                )
            })
            .unwrap_or_default()
    }
    pub fn new(name: &str) -> Result<Self> {
        Self::new_adapter(name, "", "")
    }
    /// A device for work that needs a GPU, not a particular picture: encoder
    /// probes and tools. A display that is gone is replaced by the primary.
    pub fn new_adapter(name: &str, adapter_name: &str, pnp_id: &str) -> Result<Self> {
        let choices = displays()?;
        let display = match select_display(&choices, name) {
            Ok(display) => display,
            Err(error) => {
                tracing::debug!(%error, "using the primary display's GPU");
                select_display(&choices, "")?
            }
        };
        Self::with_display(display.cloned(), adapter_name, pnp_id)
    }
    /// Capture's device: the named display and never another one, so that a
    /// stream cannot silently show a different desktop. An empty name is the
    /// primary display.
    pub fn for_display(name: &str, adapter_name: &str, pnp_id: &str) -> Result<Self> {
        let choices = displays()?;
        Self::with_display(
            select_display(&choices, name)?.cloned(),
            adapter_name,
            pnp_id,
        )
    }
    fn with_display(display: Option<Display>, adapter_name: &str, pnp_id: &str) -> Result<Self> {
        // SAFETY: The factory, adapters and outputs are live COM objects returned by these calls,
        // and each GetDesc1 returns an owned struct.
        unsafe {
            let Some(mut display) = display else {
                return Self::without_display(adapter_name, pnp_id);
            };
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let adapter = factory.EnumAdapters1(display.adapter_index)?;
            let output = Some(adapter.EnumOutputs(display.output_index)?.cast()?);
            let adapter = if adapter_name.is_empty() && pnp_id.is_empty() {
                adapter
            } else {
                let mut matches = Vec::new();
                for index in 0..32 {
                    let Ok(candidate) = factory.EnumAdapters1(index) else {
                        break;
                    };
                    let desc = candidate.GetDesc1()?;
                    if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                        continue;
                    }
                    let matched = if !pnp_id.is_empty() {
                        adapter_pnp_id(desc.AdapterLuid)
                            .is_some_and(|id| id.eq_ignore_ascii_case(pnp_id))
                    } else {
                        from_wide(&desc.Description) == adapter_name
                    };
                    if matched {
                        matches.push(candidate);
                        if pnp_id.is_empty() {
                            break;
                        }
                    }
                }
                if matches.len() != 1 {
                    bail!(
                        "configured GPU adapter was not resolved uniquely: {adapter_name} {pnp_id}"
                    );
                }
                matches.remove(0)
            };
            display.adapter = from_wide(&adapter.GetDesc1()?.Description);
            Self::create(&adapter, output, display)
        }
    }
    /// A device on the configured GPU, or the first hardware GPU, when no
    /// monitor is connected: the host can still probe its encoders, as
    /// Vibepollo does on headless hosts.
    fn without_display(adapter_name: &str, pnp_id: &str) -> Result<Self> {
        // SAFETY: The factory and adapters are live COM objects returned by these calls, and
        // GetDesc1 returns an owned struct.
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            for index in 0..32 {
                let Ok(adapter) = factory.EnumAdapters1(index) else {
                    break;
                };
                let desc = adapter.GetDesc1()?;
                let description = from_wide(&desc.Description);
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0
                    || (!pnp_id.is_empty()
                        && !adapter_pnp_id(desc.AdapterLuid)
                            .is_some_and(|id| id.eq_ignore_ascii_case(pnp_id)))
                    || (pnp_id.is_empty()
                        && !adapter_name.is_empty()
                        && description != adapter_name)
                {
                    continue;
                }
                let display = Display {
                    device_id: String::new(),
                    display_name: String::new(),
                    friendly_name: String::new(),
                    width: 0,
                    height: 0,
                    x: 0,
                    y: 0,
                    primary: false,
                    adapter: description,
                    adapter_index: index,
                    output_index: 0,
                };
                return Self::create(&adapter, None, display);
            }
            bail!("no desktop display or hardware GPU")
        }
    }
    fn create(
        adapter: &IDXGIAdapter1,
        output: Option<IDXGIOutput1>,
        display: Display,
    ) -> Result<Self> {
        // SAFETY: `adapter` is a live DXGI adapter, the out-parameters are live locals, and they
        // are unwrapped only after D3D11CreateDevice succeeded.
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let context = context.unwrap();
            let multithread: ID3D11Multithread = context.cast()?;
            let _ = multithread.SetMultithreadProtected(true);
            Ok(Self {
                device: device.unwrap(),
                context,
                output,
                display,
            })
        }
    }
}
pub struct Duplication {
    gpu: Device,
    duplicate: IDXGIOutputDuplication,
    staging: Option<ID3D11Texture2D>,
    owned: GpuPool,
    last_desktop: Option<GpuImage>,
    cursor: crate::cursor::State,
    frame_owned: bool,
}
impl Duplication {
    pub fn new(name: &str) -> Result<Self> {
        Self::new_format(name, false)
    }
    pub fn new_format(name: &str, hdr: bool) -> Result<Self> {
        Self::new_device(Device::new(name)?, hdr)
    }
    fn new_device(gpu: Device, hdr: bool) -> Result<Self> {
        let output = gpu.output()?.clone();
        // Desktop Duplication works only on the GPU the display is connected
        // to; WGC also captures it for a device on another GPU.
        if let (Ok(device), Ok(owner)) = (
            gpu.device
                .cast::<IDXGIDevice>()
                // SAFETY: `d` is the device's live IDXGIDevice, and GetAdapter returns an owned
                // adapter.
                .and_then(|d| unsafe { d.GetAdapter() }),
            // SAFETY: `output` is a live IDXGIOutput1, and GetParent returns an owned reference.
            unsafe { output.GetParent::<IDXGIAdapter>() },
            // SAFETY: Both adapters are live COM objects, and GetDesc returns owned structs.
        ) && let (Ok(device), Ok(owner)) = unsafe { (device.GetDesc(), owner.GetDesc()) }
            && (device.AdapterLuid.LowPart, device.AdapterLuid.HighPart)
                != (owner.AdapterLuid.LowPart, owner.AdapterLuid.HighPart)
        {
            bail!(
                "Desktop Duplication captures {} only on the GPU it is connected to ({}), not on {}; use WGC capture to encode on another GPU",
                gpu.display.display_name,
                from_wide(&owner.Description),
                from_wide(&device.Description)
            );
        }
        let native_hdr = hdr
            && output
                .cast::<IDXGIOutput6>()
                // SAFETY: `output` is a live IDXGIOutput6, and GetDesc1 returns an owned struct.
                .and_then(|output| unsafe { output.GetDesc1() })
                .is_ok_and(|desc| desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);
        // Always name the formats this host accepts. The legacy call loses
        // access in a loop whenever Windows composes the output in FP16, as it
        // does for advanced color or after another virtual display arrives.
        let formats: &[DXGI_FORMAT] = if native_hdr {
            &[DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_B8G8R8A8_UNORM]
        } else {
            &[DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT]
        };
        // SAFETY: `output` and `gpu.device` are live, and `formats` outlives the DuplicateOutput1
        // call that reads it.
        let (duplicate, api) = unsafe {
            // DuplicateOutput1 needs a per-monitor DPI-aware process; fall back
            // to the legacy call rather than fail where it is refused.
            match output
                .cast::<IDXGIOutput5>()
                .and_then(|output| output.DuplicateOutput1(&gpu.device, 0, formats))
            {
                Ok(duplicate) => (duplicate, "DuplicateOutput1"),
                Err(error) => {
                    tracing::warn!(%error, "DuplicateOutput1 unavailable; trying legacy Desktop Duplication, which may lose HDR or fail after display changes. Update Windows and the graphics driver or select WGC");
                    (
                        output.DuplicateOutput(&gpu.device).with_context(|| {
                            format!("opening legacy Desktop Duplication after DuplicateOutput1 failed: {error}")
                        })?,
                        "DuplicateOutput",
                    )
                }
            }
        };
        // SAFETY: `duplicate` is a live output duplication, and GetDesc returns an owned struct.
        let desc = unsafe { duplicate.GetDesc() };
        tracing::info!(api, output = %gpu.display.display_name,
            width = desc.ModeDesc.Width, height = desc.ModeDesc.Height,
            x = gpu.display.x, y = gpu.display.y, rotation = desc.Rotation.0,
            format = desc.ModeDesc.Format.0, "Desktop Duplication opened");
        Ok(Self {
            gpu,
            duplicate,
            staging: None,
            owned: GpuPool::default(),
            last_desktop: None,
            cursor: Default::default(),
            frame_owned: false,
        })
    }
    pub fn next(&mut self, timeout: Duration) -> Result<Option<Image>> {
        let Some((_, resource)) = self.acquire_frame(timeout.as_millis().min(100) as u32)? else {
            return Ok(None);
        };
        // CPU and GPU callers may alternate on the same duplication object.
        // A CPU acquisition cannot leave an older GPU desktop cache reusable.
        self.last_desktop = None;
        let result = (|| -> Result<Image> {
            let texture: ID3D11Texture2D = resource.context("empty captured texture")?.cast()?;
            read_texture(&self.gpu, &texture, &mut self.staging)
        })();
        if result.is_err() {
            let _ = self.release_frame();
        }
        result.map(Some)
    }
    pub fn next_gpu(&mut self) -> Result<Option<GpuImage>> {
        // Keep acquisition nonblocking. A waiting acquisition holds the device's
        // lock, so another thread's D3D11 call took up to 42 ms meanwhile, and
        // beside a game it delivered fewer pictures, later, than polling
        // (`examples/ddx_arrival_probe.rs`).
        let Some((info, resource)) = self.acquire_frame(0)? else {
            return Ok(None);
        };
        let shown = self.cursor.shown();
        let result = (|| {
            self.cursor.update(&self.gpu, &self.duplicate, &info)?;
            // A pointer-only update while the pointer stays hidden (mouse-look
            // in a game) changes nothing on screen. Publishing the picture
            // again spent the pacing slot of the game's next frame. The frame
            // is released at the next acquisition.
            if info.LastPresentTime == 0
                && self.last_desktop.is_some()
                && !shown
                && !self.cursor.shown()
            {
                return Ok(None);
            }
            let texture: ID3D11Texture2D = resource.context("empty captured texture")?.cast()?;
            let mut image = self.owned.desktop_snapshot(
                &self.gpu,
                &texture,
                &mut self.last_desktop,
                info.LastPresentTime != 0,
            )?;
            if let Some(image) = image.as_mut() {
                image.captured =
                    qpc_timestamps(info.LastPresentTime.max(info.LastMouseUpdateTime)).0;
                image.acquired = Instant::now();
                image.cursor = self.cursor.snapshot();
            }
            Ok(image)
        })();
        if result.is_err() {
            let _ = self.release_frame();
        }
        result
    }
    fn release_frame(&mut self) -> Result<()> {
        if std::mem::replace(&mut self.frame_owned, false) {
            // SAFETY: `frame_owned` was set, so this duplication holds an acquired frame that has
            // not been released.
            unsafe {
                self.duplicate.ReleaseFrame()?;
            }
        }
        Ok(())
    }
    fn acquire_frame(
        &mut self,
        timeout_ms: u32,
    ) -> Result<Option<(DXGI_OUTDUPL_FRAME_INFO, Option<IDXGIResource>)>> {
        // Release immediately before acquisition, as recommended by DXGI.
        // Between polls, Windows tracks dirty regions instead of repeatedly
        // copying desktop updates into a surface we have already snapshotted.
        self.release_frame()?;
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        // SAFETY: `duplicate` is live, the previous frame was released above, and both
        // out-parameters are live locals.
        match unsafe {
            self.duplicate
                .AcquireNextFrame(timeout_ms, &mut info, &mut resource)
        } {
            Ok(()) => {
                self.frame_owned = true;
                Ok(Some((info, resource)))
            }
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}
impl Drop for Duplication {
    fn drop(&mut self) {
        let _ = self.release_frame();
    }
}

/// An immutable, owned GPU snapshot. The capture pool never overwrites a
/// texture until the latest-frame slot and every encoder have released it.
#[derive(Clone)]
pub struct GpuImage {
    pub(crate) cursor: Option<crate::cursor::Cursor>,
    pub width: u32,
    pub height: u32,
    pub pixel: Pixel,
    /// Capture timestamp used by pacing and encoders. Future, missing or
    /// more than two-second-old stamps fall back to the conversion time.
    pub captured: Instant,
    /// Unclamped WGC SystemRelativeTime, for diagnostics only. This is not
    /// an application Present time and can be later than host acquisition.
    pub wgc_stamp: Option<Instant>,
    /// When the host submitted its owned copy, before GPU completion.
    pub acquired: Instant,
    pub gpu: Device,
    pub texture: std::sync::Arc<ID3D11Texture2D>,
    /// When a compute-queue copy into `texture` completes. D3D11 work on
    /// `gpu` is already ordered after it; D3D12 queues wait for this.
    pub(crate) ready: Option<crate::compute::Ready>,
    /// A WGC helper texture read in place rather than copied; it goes back
    /// to the helper with the last clone of this image.
    lease: Option<std::sync::Arc<bridge::Lease>>,
}
impl GpuImage {
    pub fn readback(&self, staging: &mut Option<ID3D11Texture2D>) -> Result<Image> {
        let mut image = read_texture(&self.gpu, &self.texture, staging)?;
        image.captured = self.captured;
        if let Some(cursor) = &self.cursor {
            cursor.blend(&mut image);
        }
        Ok(image)
    }
    pub fn upload(gpu: &Device, image: &Image) -> Result<Self> {
        let format = match image.pixel {
            Pixel::Bgra8 => DXGI_FORMAT_B8G8R8A8_UNORM,
            Pixel::RgbaF16 => DXGI_FORMAT_R16G16B16A16_FLOAT,
            Pixel::Rgba10Pq => DXGI_FORMAT_R10G10B10A2_UNORM,
        };
        let bytes = if image.pixel == Pixel::RgbaF16 { 8 } else { 4 };
        if image.width == 0
            || image.height == 0
            || image.stride < image.width as usize * bytes
            || image.bytes.len() < image.stride * image.height as usize
        {
            bail!("invalid GPU upload image");
        }
        // SAFETY: `image.bytes` holds `stride * height` bytes with `stride` covering a row (checked
        // above), and CreateTexture2D copies them during the call.
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: image.width,
                Height: image.height,
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                // Shareable with the D3D12 compute converter.
                MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED.0)
                    as u32,
                ..Default::default()
            };
            let data = D3D11_SUBRESOURCE_DATA {
                pSysMem: image.bytes.as_ptr().cast(),
                SysMemPitch: image.stride.try_into()?,
                SysMemSlicePitch: 0,
            };
            let mut texture = None;
            gpu.device
                .CreateTexture2D(&desc, Some(&data), Some(&mut texture))?;
            Ok(Self {
                cursor: None,
                width: image.width,
                height: image.height,
                pixel: image.pixel,
                captured: image.captured,
                wgc_stamp: None,
                acquired: Instant::now(),
                gpu: gpu.clone(),
                texture: std::sync::Arc::new(texture.unwrap()),
                ready: None,
                lease: None,
            })
        }
    }
}

#[derive(Default)]
pub(crate) struct GpuPool {
    pub(crate) warnings: std::sync::Arc<butterpollo_core::session::Warnings>,
    textures: Vec<std::sync::Arc<ID3D11Texture2D>>,
    /// Copies on a compute queue instead of the D3D11 graphics queue,
    /// where they wait behind a game's rendering.
    pub(crate) compute: Option<crate::compute::Handoff>,
}
impl GpuPool {
    fn desktop_snapshot(
        &mut self,
        gpu: &Device,
        source: &ID3D11Texture2D,
        cached: &mut Option<GpuImage>,
        desktop_updated: bool,
    ) -> Result<Option<GpuImage>> {
        // DXGI supplies a zero LastPresentTime for pointer-only updates. The
        // owned bitmap is still valid; only the detached cursor snapshot changes.
        if !desktop_updated && let Some(image) = cached.as_ref() {
            return Ok(Some(image.clone()));
        }
        // A missed desktop copy must invalidate the cache. Otherwise a later
        // pointer-only frame could publish pixels from before the missed update.
        *cached = None;
        let image = self.copy(gpu, source)?;
        *cached = image.clone();
        Ok(image)
    }
    pub(crate) fn copy(
        &mut self,
        gpu: &Device,
        source: &ID3D11Texture2D,
    ) -> Result<Option<GpuImage>> {
        // SAFETY: `source` and the pool's textures are live on `gpu.device` with matching size and
        // format, and that device's context is multithread-protected.
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            source.GetDesc(&mut desc);
            let pixel = match desc.Format {
                DXGI_FORMAT_B8G8R8A8_UNORM => Pixel::Bgra8,
                DXGI_FORMAT_R16G16B16A16_FLOAT => Pixel::RgbaF16,
                DXGI_FORMAT_R10G10B10A2_UNORM => Pixel::Rgba10Pq,
                _ => bail!("unsupported GPU capture format {:?}", desc.Format),
            };
            self.textures.retain(|texture| {
                let mut old = D3D11_TEXTURE2D_DESC::default();
                texture.GetDesc(&mut old);
                old.Width == desc.Width && old.Height == desc.Height && old.Format == desc.Format
            });
            let free = self
                .textures
                .iter()
                .find(|texture| std::sync::Arc::strong_count(texture) == 1)
                .cloned();
            let texture = if let Some(texture) = free {
                texture
            } else {
                // A slow consumer drops capture updates instead of growing a frame queue.
                if self.textures.len() >= 8 {
                    return Ok(None);
                }
                desc.Usage = D3D11_USAGE_DEFAULT;
                desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
                desc.CPUAccessFlags = 0;
                // Shared with the compute queue that copies and converts it.
                desc.MiscFlags = if self.compute.is_some() {
                    (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED.0) as u32
                } else {
                    0
                };
                let mut texture = None;
                gpu.device
                    .CreateTexture2D(&desc, None, Some(&mut texture))?;
                let texture = std::sync::Arc::new(texture.unwrap());
                self.textures.push(texture.clone());
                texture
            };
            let ready = match self
                .compute
                .as_mut()
                .map(|handoff| handoff.copy(texture.as_ref(), source))
            {
                Some(Ok(ready)) => Some(ready),
                Some(Err(error)) => {
                    if let Some(loss) = crate::device_loss::DeviceLost::d3d11(&gpu.device)
                        .or_else(|| {
                            self.compute
                                .as_ref()
                                .and_then(|handoff| handoff.device_removed())
                        })
                        .or_else(|| crate::device_loss::DeviceLost::from_error(&error))
                    {
                        return Err(error.context(loss));
                    }
                    // Replace the shared textures and copy this same frame on
                    // D3D11. Waiting for another update can strand a stream on
                    // a static desktop. Consumers see an unshared texture and
                    // rebuild for the graphics queue.
                    self.warnings.set("capture_compute", format!("Compute capture copy failed ({error:#}); copying on the graphics queue. A busy game can delay capture; lower GPU load or update the AMD driver."));
                    self.compute = None;
                    self.textures.clear();
                    return self.copy(gpu, source);
                }
                None => {
                    gpu.context.CopyResource(texture.as_ref(), source);
                    None
                }
            };
            Ok(Some(GpuImage {
                cursor: None,
                width: desc.Width,
                height: desc.Height,
                pixel,
                captured: Instant::now(),
                wgc_stamp: None,
                acquired: Instant::now(),
                gpu: gpu.clone(),
                texture,
                ready,
                lease: None,
            }))
        }
    }
}
pub fn read_texture(
    gpu: &Device,
    texture: &ID3D11Texture2D,
    staging: &mut Option<ID3D11Texture2D>,
) -> Result<Image> {
    // SAFETY: `stage` matches `texture`'s size and format, and each row read stays inside the
    // `RowPitch * Height` mapped bytes until Unmap below.
    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        let pixel = match desc.Format {
            DXGI_FORMAT_B8G8R8A8_UNORM => Pixel::Bgra8,
            DXGI_FORMAT_R16G16B16A16_FLOAT => Pixel::RgbaF16,
            DXGI_FORMAT_R10G10B10A2_UNORM => Pixel::Rgba10Pq,
            _ => bail!("unsupported capture texture format {:?}", desc.Format),
        };
        let mut needs = true;
        if let Some(old) = staging {
            let mut d = D3D11_TEXTURE2D_DESC::default();
            old.GetDesc(&mut d);
            needs = d.Width != desc.Width || d.Height != desc.Height || d.Format != desc.Format;
        }
        if needs {
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut created = None;
            gpu.device
                .CreateTexture2D(&desc, None, Some(&mut created))?;
            *staging = created;
        }
        let stage = staging.as_ref().unwrap();
        gpu.context.CopyResource(stage, texture);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.context
            .Map(stage, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let stride = (desc.Width as usize) * if pixel == Pixel::RgbaF16 { 8 } else { 4 };
        let mut bytes = vec![0; stride * desc.Height as usize];
        for y in 0..desc.Height as usize {
            std::ptr::copy_nonoverlapping(
                (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                bytes.as_mut_ptr().add(y * stride),
                stride,
            );
        }
        gpu.context.Unmap(stage, 0);
        Ok(Image {
            width: desc.Width,
            height: desc.Height,
            stride,
            bytes,
            captured: Instant::now(),
            pixel,
        })
    }
}
pub struct ClaimGrid {
    pub anchor: Instant,
    pub period: Duration,
}
fn nanos(at: Instant, origin: Instant) -> i64 {
    if at >= origin {
        at.duration_since(origin).as_nanos().min(i64::MAX as u128) as i64
    } else {
        -(origin.duration_since(at).as_nanos().min(i64::MAX as u128) as i64)
    }
}
const WGC_FRAME_POOL_CAPACITY: usize = 2;

/// Select only within the current pool's capacity so a busy producer cannot
/// keep capture draining forever. Each acquired frame is validated, including
/// ones that would be skipped, so resize/access errors still trigger recovery.
fn newest_capture_frame<T>(
    limit: usize,
    mut next: impl FnMut() -> Result<Option<T>>,
    mut validate: impl FnMut(&T) -> Result<()>,
    mut close: impl FnMut(T) -> Result<()>,
) -> Result<Option<T>> {
    let mut newest = None;
    for _ in 0..limit {
        let candidate = match next() {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                if let Some(frame) = newest {
                    let _ = close(frame);
                }
                return Err(error);
            }
        };
        if let Err(error) = validate(&candidate) {
            let _ = close(candidate);
            if let Some(frame) = newest {
                let _ = close(frame);
            }
            return Err(error);
        }
        if let Some(previous) = newest.replace(candidate)
            && let Err(error) = close(previous)
        {
            if let Some(frame) = newest.take() {
                let _ = close(frame);
            }
            return Err(error);
        }
    }
    Ok(newest)
}

pub struct Wgc {
    gpu: Device,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    notifications: Option<(std::sync::Arc<crate::timing::Signal>, i64)>,
    staging: Option<ID3D11Texture2D>,
    owned: GpuPool,
    grid: Option<std::sync::Arc<std::sync::Mutex<ClaimGrid>>>,
    intervals: butterpollo_core::capture_policy::Intervals,
    origin: Instant,
    last_publish: Instant,
    held: Option<(Direct3D11CaptureFrame, Instant, Instant, Option<Instant>)>,
    size: (i32, i32),
    color_space: Option<DXGI_COLOR_SPACE_TYPE>,
    color_check: Instant,
    drain_to_newest: bool,
    /// The captured display. When it leaves the desktop, even for a moment
    /// (a layout change switching it off and on), its capture item closes and
    /// WGC delivers no more frames, without an error.
    item: GraphicsCaptureItem,
    /// Its HMONITOR, kept as a number so the capture can move threads.
    monitor: usize,
    closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    closed_token: Option<i64>,
}
pub(crate) fn qpc_frequency() -> i64 {
    *std::sync::OnceLock::get_or_init(&QPC_FREQUENCY, || {
        let mut frequency = 0;
        // SAFETY: QueryPerformanceFrequency writes only to the live local `frequency`.
        let _ = unsafe {
            windows::Win32::System::Performance::QueryPerformanceFrequency(&mut frequency)
        };
        frequency.max(1)
    })
}
static QPC_FREQUENCY: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
fn qpc_timestamps(ticks: i64) -> (Instant, Option<Instant>) {
    let now = Instant::now();
    let mut current = 0;
    if ticks <= 0
        // SAFETY: QueryPerformanceCounter writes only to the live local `current`.
        || unsafe { windows::Win32::System::Performance::QueryPerformanceCounter(&mut current) }
            .is_err()
    {
        return (now, None);
    }
    qpc_timestamps_at(ticks, current, qpc_frequency(), now)
}
fn qpc_timestamps_at(
    ticks: i64,
    current: i64,
    frequency: i64,
    now: Instant,
) -> (Instant, Option<Instant>) {
    if ticks <= 0 || current <= 0 || frequency <= 0 {
        return (now, None);
    }
    let delta = Duration::from_secs_f64(ticks.abs_diff(current) as f64 / frequency as f64);
    let stamp = if ticks > current {
        now.checked_add(delta)
    } else {
        now.checked_sub(delta)
    };
    // Keep the historical clock for pacing; diagnostics retain the raw stamp.
    let captured = if ticks > current || delta > Duration::from_secs(2) {
        now
    } else {
        stamp.unwrap_or(now)
    };
    (captured, stamp)
}
fn wgc_qpc(ticks: i64, frequency: i64) -> i64 {
    (i128::from(ticks) * i128::from(frequency) / 10_000_000).clamp(0, i128::from(i64::MAX)) as i64
}
fn wgc_presentation(frame: &Direct3D11CaptureFrame) -> (Instant, Option<Instant>) {
    let ticks = frame.SystemRelativeTime().map(|t| t.Duration).unwrap_or(0);
    qpc_timestamps(wgc_qpc(ticks, qpc_frequency()))
}
fn pin_capture_runtime() -> Result<()> {
    use std::sync::OnceLock;
    use windows::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_PIN, GetModuleHandleExW,
    };
    static PINNED: OnceLock<windows::core::Result<()>> = OnceLock::new();
    // Closing a free-threaded pool does not join all of GraphicsCapture's workers.
    // COM teardown can unload the DLL while one is still returning through it:
    // https://github.com/robmikh/Win32CaptureSample/issues/99
    // Keep only the system runtime loaded for the process lifetime; sessions,
    // frame pools and their GPU resources still close and release normally.
    PINNED
        // SAFETY: The module name is a static NUL-terminated literal, and `module` is a live local.
        .get_or_init(|| unsafe {
            let mut module = HMODULE::default();
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_PIN,
                windows::core::w!("GraphicsCapture.dll"),
                &mut module,
            )
        })
        .clone()
        .context("keep the Windows capture runtime loaded during asynchronous shutdown")
}
impl Wgc {
    pub fn new(name: &str) -> Result<Self> {
        Self::new_format(name, false)
    }
    pub fn new_format(name: &str, hdr: bool) -> Result<Self> {
        Self::new_device(Device::new(name)?, hdr, false)
    }
    fn new_device(gpu: Device, hdr: bool, high_rate: bool) -> Result<Self> {
        // SAFETY: `output` and `gpu.device` are live, `d.Monitor` is that output's monitor, and
        // every call returns owned COM or WinRT objects.
        unsafe {
            let output = gpu.output()?.clone();
            let d = output.GetDesc()?;
            let color_space = output
                .cast::<IDXGIOutput6>()
                .and_then(|output| output.GetDesc1())
                .map(|desc| desc.ColorSpace)
                .ok();
            let native_hdr = color_space
                .map(|space| space == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)
                .unwrap_or(hdr);
            let interop: IGraphicsCaptureItemInterop =
                windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                    .context("Windows Graphics Capture is unavailable")?;
            pin_capture_runtime()?;
            let item: GraphicsCaptureItem = interop
                .CreateForMonitor(d.Monitor)
                .context("Windows Graphics Capture cannot capture this display")?;
            let closed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = closed.clone();
            let closed_token = item
                .Closed(&windows::Foundation::TypedEventHandler::new(move |_, _| {
                    flag.store(true, std::sync::atomic::Ordering::Release);
                    Ok(())
                }))
                .inspect_err(|error| tracing::warn!(%error, "WGC cannot report the display leaving; a capture that stops is found by its monitor check"))
                .ok();
            let dxgi: IDXGIDevice = gpu.device.cast()?;
            let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
            let size = item.Size()?;
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                &winrt,
                if native_hdr {
                    DirectXPixelFormat::R16G16B16A16Float
                } else {
                    DirectXPixelFormat::B8G8R8A8UIntNormalized
                },
                WGC_FRAME_POOL_CAPACITY as i32,
                size,
            )
            .context("Windows Graphics Capture frame pool")?;
            let session = pool
                .CreateCaptureSession(&item)
                .context("Windows Graphics Capture session")?;
            session.SetIsCursorCaptureEnabled(true)?;
            let _ = session.SetIsBorderRequired(false);
            // Leaving this property untouched is not equivalent to zero:
            // Windows build 26200 reports a 16 ms default, which can deliver
            // only 50-57 updates/sec when quantized to compositor ticks.
            // Explicitly remove that throttle at every stream rate. The old
            // 1 ms workaround remains available for diagnostic comparisons.
            let interval = windows::Foundation::TimeSpan {
                Duration: if high_rate { 10_000 } else { 0 },
            };
            match session.SetMinUpdateInterval(interval) {
                Ok(()) => tracing::info!(
                    interval_us = interval.Duration / 10,
                    "WGC minimum update interval configured"
                ),
                Err(error) => {
                    tracing::warn!(%error, "WGC minimum update interval unavailable; using the Windows default, which may limit capture fps. Update Windows or disable high-rate capture")
                }
            }
            let capture = Self {
                gpu,
                pool,
                session,
                notifications: None,
                staging: None,
                owned: GpuPool::default(),
                grid: None,
                intervals: Default::default(),
                origin: Instant::now(),
                last_publish: Instant::now(),
                held: None,
                size: (size.Width, size.Height),
                color_space,
                color_check: Instant::now() + Duration::from_secs(1),
                drain_to_newest: false,
                item,
                monitor: d.Monitor.0 as usize,
                closed,
                closed_token,
            };
            // Own the pool/session before starting so failure closes them
            // just like normal capture teardown.
            capture
                .session
                .StartCapture()
                .context("Windows Graphics Capture did not start")?;
            Ok(capture)
        }
    }
    fn enable_notifications(&mut self) -> Result<()> {
        if self.notifications.is_none() {
            let arrived = std::sync::Arc::new(crate::timing::Signal::new()?);
            let wake = arrived.clone();
            // The callback never takes the D3D11 lock or touches capture frames.
            let token = self
                .pool
                .FrameArrived(&windows::Foundation::TypedEventHandler::new(move |_, _| {
                    let _ = wake.set();
                    Ok(())
                }))?;
            self.notifications = Some((arrived.clone(), token));
            // Registration may follow the first frame; make the caller check
            // the pool before waiting for a subsequent notification.
            arrived.set()?;
        }
        Ok(())
    }
    pub fn next_frame(&mut self) -> Result<Option<Image>> {
        self.check_color_space()?;
        let Some(frame) = self.next_native_frame()? else {
            return Ok(None);
        };
        let result = (|| -> Result<Image> {
            let surface = frame.Surface()?;
            let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
            // SAFETY: `access` is the surface of `frame`, which is closed only after this closure.
            let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
            read_texture(&self.gpu, &texture, &mut self.staging)
        })();
        frame.Close()?;
        result.map(Some)
    }
    pub fn next_gpu(&mut self) -> Result<Option<GpuImage>> {
        self.check_color_space()?;
        let now = Instant::now();
        if let Some(frame) = self.next_native_frame()? {
            let composition = self.intervals.observe(nanos(now, self.origin));
            if let Some((previous, _, _, _)) = self.held.take()
                && let Err(error) = previous.Close()
            {
                let _ = frame.Close();
                return Err(error.into());
            }
            let deadline = self.grid.as_ref().and_then(|grid| {
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
            });
            let (presented, stamp) = wgc_presentation(&frame);
            self.held = Some((frame, deadline.unwrap_or(now), presented, stamp));
        }
        if self
            .held
            .as_ref()
            .is_none_or(|(_, deadline, _, _)| *deadline > now)
        {
            return Ok(None);
        }
        let (frame, _, captured, stamp) = self.held.take().unwrap();
        let result = (|| {
            let surface = frame.Surface()?;
            let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
            // SAFETY: `access` is the surface of `frame`, which is closed only after this closure.
            let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
            let mut image = self.owned.copy(&self.gpu, &texture)?;
            if let Some(image) = image.as_mut() {
                image.captured = captured;
                image.wgc_stamp = stamp;
            }
            Ok(image)
        })();
        frame.Close()?;
        self.last_publish = now;
        result
    }
    fn next_native_frame(&self) -> Result<Option<Direct3D11CaptureFrame>> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            bail!("the captured display left the desktop");
        }
        newest_capture_frame(
            if self.drain_to_newest {
                WGC_FRAME_POOL_CAPACITY
            } else {
                1
            },
            || self.try_frame(),
            |frame| self.check_size(frame),
            |frame| frame.Close().map_err(Into::into),
        )
    }
    fn try_frame(&self) -> Result<Option<Direct3D11CaptureFrame>> {
        // Reset before checking the pool: a notification racing the check stays
        // set, and a frame queued before the reset is found by TryGetNextFrame.
        // Resetting after an empty result would lose an arriving frame's wakeup.
        if let Some((arrived, _)) = &self.notifications {
            arrived.reset()?;
        }
        // An empty pool returns a successful HRESULT and a null interface. The
        // generated binding requires a non-null frame, so preserve the HRESULT
        // and optional output separately instead of swallowing every error.
        // SAFETY: `pool` is live, the call matches the TryGetNextFrame vtable slot, and a non-null
        // `frame` is a reference it transferred to us.
        unsafe {
            let mut frame = std::ptr::null_mut();
            (self.pool.vtable().TryGetNextFrame)(self.pool.as_raw(), &mut frame)
                .ok()
                .context("Windows capture frame pool failed; reconnect required")?;
            Ok(if frame.is_null() {
                None
            } else {
                // A successful call transfers this frame reference to us.
                Some(Direct3D11CaptureFrame::from_raw(frame))
            })
        }
    }
    fn check_size(&self, frame: &Direct3D11CaptureFrame) -> Result<()> {
        let size = frame.ContentSize()?;
        if (size.Width, size.Height) != self.size {
            bail!("capture output dimensions changed");
        }
        Ok(())
    }
    fn check_color_space(&mut self) -> Result<()> {
        if Instant::now() < self.color_check {
            return Ok(());
        }
        self.color_check = Instant::now() + Duration::from_secs(1);
        if let Some(loss) = crate::device_loss::DeviceLost::d3d11(&self.gpu.device) {
            return Err(loss.into());
        }
        // A display that returned has a new monitor handle; the old one's
        // capture stays silent, so reopen on the display that is there now.
        let mut info = windows::Win32::Graphics::Gdi::MONITORINFO {
            cbSize: size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `info` is a live local whose size is set; a stale handle only fails the call.
        if !unsafe {
            windows::Win32::Graphics::Gdi::GetMonitorInfoW(
                windows::Win32::Graphics::Gdi::HMONITOR(self.monitor as *mut _),
                &mut info,
            )
        }
        .as_bool()
        {
            bail!("the captured display left the desktop");
        }
        // SAFETY: `output` is a live IDXGIOutput1, and GetDesc1 returns an owned struct.
        let current = unsafe {
            self.gpu
                .output()
                .map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))
                .and_then(|output| output.cast::<IDXGIOutput6>())
                .and_then(|output| output.GetDesc1())
        }
        .map(|desc| desc.ColorSpace)
        .ok();
        if current.is_some() && current != self.color_space {
            bail!("capture output color space changed");
        }
        Ok(())
    }
    fn close(&mut self) {
        // Windows can fail-fast instead of returning RO_E_CLOSED when an event
        // is revoked after pool.Close(). Always revoke first and only once.
        // An in-flight callback owns its own Arc to the event until it returns.
        if let Some((_, token)) = self.notifications.take() {
            let _ = self.pool.RemoveFrameArrived(token);
        }
        if let Some(token) = self.closed_token.take() {
            let _ = self.item.RemoveClosed(token);
        }
        if let Some((frame, _, _, _)) = self.held.take() {
            let _ = frame.Close();
        }
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

impl Drop for Wgc {
    fn drop(&mut self) {
        self.close();
    }
}
mod bridge;
pub use bridge::run_worker as run_wgc_worker;

/// Whether WGC can capture the input desktop without crossing into Winlogon.
pub fn wgc_desktop_available() -> bool {
    bridge::desktop_available()
}

pub enum Capture {
    Wgc(Box<Wgc>),
    WgcWorker(Box<bridge::Session>),
    Dxgi(Box<Duplication>),
    /// Holds no device: the place of a lost capture while a new one is made.
    Closed,
}
/// The desktop pointer a lost capture last saw, for the capture replacing it.
pub struct Pointer(crate::cursor::Carried);

fn open_stream_capture<T>(
    kind: &str,
    warnings: &butterpollo_core::session::Warnings,
    mut open: impl FnMut(&str) -> Result<T>,
) -> Result<T> {
    match open(kind) {
        Ok(capture) => {
            if matches!(kind, "wgc" | "ddx" | "dxgi") {
                warnings.clear("capture_backend");
            }
            Ok(capture)
        }
        Err(error) if crate::device_loss::DeviceLost::from_error(&error).is_some() => Err(error),
        Err(error) if kind == "wgc" => {
            // WGC can be unavailable under SYSTEM or on a secure desktop.
            // Apply the same fallback at startup and after a capture restart.
            let capture = open("ddx").with_context(|| {
                format!("WGC failed ({error:#}); Desktop Duplication fallback also failed")
            })?;
            warnings.set("capture_backend", format!("Capturing with Desktop Duplication: WGC unavailable ({error:#}). Capture timing and HDR may differ. Unlock the desktop or dismiss the UAC prompt; if this persists, check the WGC helper and graphics driver."));
            Ok(capture)
        }
        Err(error) => Err(error),
    }
}

impl Capture {
    /// The pointer to hand to the capture that replaces this one.
    pub fn pointer(&self) -> Option<Pointer> {
        match self {
            Self::Dxgi(duplication) => Some(Pointer(duplication.cursor.carry())),
            _ => None,
        }
    }
    /// Draw `pointer` until Desktop Duplication reports the pointer itself,
    /// which it does for a shape only when the shape changes.
    pub fn resume_pointer(&mut self, pointer: Pointer) -> Result<()> {
        if let Self::Dxgi(duplication) = self {
            let Duplication { gpu, cursor, .. } = &mut **duplication;
            cursor.resume(gpu, pointer.0)?;
        }
        Ok(())
    }
    pub fn backend(&self) -> &'static str {
        match self {
            Self::Wgc(_) | Self::WgcWorker(_) => "wgc",
            Self::Dxgi(_) => "ddx",
            Self::Closed => "closed",
        }
    }
    pub fn set_claim_grid(
        &mut self,
        grid: std::sync::Arc<std::sync::Mutex<ClaimGrid>>,
        enabled: bool,
    ) {
        if let Self::Wgc(wgc) = self {
            wgc.grid = enabled.then_some(grid);
        } else if let Self::WgcWorker(worker) = self {
            worker.set_claim_grid(enabled.then_some(grid));
        }
    }
    pub fn publication_deadline(&self) -> Option<Instant> {
        match self {
            Self::Wgc(wgc) => wgc.held.as_ref().map(|(_, deadline, _, _)| *deadline),
            Self::WgcWorker(worker) => worker.publication_deadline(),
            _ => None,
        }
    }
    /// Set when the user-session capture helper announces a frame. A wait for
    /// the next poll that also ends on it sees the frame at once instead of
    /// up to a poll interval later; the polling itself stays.
    pub fn frame_signal(&self) -> Option<&crate::timing::Signal> {
        match self {
            Self::WgcWorker(worker) => Some(worker.frame_signal()),
            _ => None,
        }
    }
    /// Opt in to notifications for capture probes. Normal streams keep polling:
    /// pure event waits measured slower detection under GPU load on the test PC.
    pub fn enable_frame_notifications(&mut self) -> Result<()> {
        match self {
            Self::Wgc(wgc) => wgc.enable_notifications(),
            _ => bail!("frame notifications require Windows Graphics Capture"),
        }
    }
    /// Wait on an enabled WGC notification or the probe's deadline.
    pub fn wait_until(&self, timer: &crate::timing::Timer, deadline: Instant) -> Result<()> {
        if let Self::Wgc(wgc) = self
            && let Some((arrived, _)) = &wgc.notifications
        {
            timer.until_or_signal(deadline, arrived)?;
        } else {
            timer.until(deadline);
        }
        Ok(())
    }
    /// Keep a stream usable when WGC cannot open. Explicit capture probes use
    /// `new_options` instead, so a WGC benchmark cannot silently measure DDX.
    pub fn open_for_stream_reported(
        name: &str,
        kind: &str,
        hdr: bool,
        config: &butterpollo_core::config::Config,
        warnings: std::sync::Arc<butterpollo_core::session::Warnings>,
    ) -> Result<Self> {
        warnings.clear("capture_compute");
        warnings.clear("capture_helper_compute");
        open_stream_capture(kind, &warnings, |backend| {
            Self::new_options_reported(name, backend, hdr, config, warnings.clone())
        })
    }
    pub fn new(name: &str, kind: &str) -> Result<Self> {
        Self::new_format(name, kind, false)
    }
    pub fn new_format(name: &str, kind: &str, hdr: bool) -> Result<Self> {
        Self::new_options(name, kind, hdr, &Default::default())
    }
    pub fn new_options(
        name: &str,
        kind: &str,
        hdr: bool,
        config: &butterpollo_core::config::Config,
    ) -> Result<Self> {
        Self::new_options_reported(name, kind, hdr, config, Default::default())
    }
    fn new_options_reported(
        name: &str,
        kind: &str,
        hdr: bool,
        config: &butterpollo_core::config::Config,
        warnings: std::sync::Arc<butterpollo_core::session::Warnings>,
    ) -> Result<Self> {
        if kind == "wgc"
            && (crate::process::is_system() || config.boolean("wgc_user_helper", false))
        {
            return Ok(Self::WgcWorker(Box::new(bridge::Session::new(
                name, hdr, config, warnings,
            )?)));
        }
        let gpu = Device::for_display(
            name,
            config.get("adapter_name", ""),
            config.get("adapter_pnp_id", ""),
        )?;
        if let Err(error) = crate::gpu_priority::configure(&gpu, config) {
            tracing::debug!(%error, "GPU priority remains at the process default");
        }
        // Desktop Duplication copies each frame out of the shared desktop
        // surface; on a compute queue that copy keeps pace beside a game.
        let duplication = |gpu: Device| -> Result<Duplication> {
            let mut duplication = Duplication::new_device(gpu, hdr)?;
            duplication.owned.warnings = warnings.clone();
            if crate::compute::enabled(config) && crate::compute::copies_on(&duplication.gpu.device)
            {
                match crate::compute::Compute::for_device(&duplication.gpu.device)
                    .and_then(|compute| crate::compute::Handoff::new(compute, &duplication.gpu))
                {
                    Ok(compute) => duplication.owned.compute = Some(compute),
                    Err(error) => {
                        warnings.set("capture_compute", format!("Compute capture copies unavailable ({error:#}); copying on the graphics queue. Lower game GPU load or update the AMD driver if capture stutters."))
                    }
                }
            }
            Ok(duplication)
        };
        let wgc = |gpu: Device| -> Result<Wgc> {
            let mut capture =
                Wgc::new_device(gpu, hdr, config.boolean("wgc_high_rate_capture", false))?;
            capture.owned.warnings = warnings.clone();
            capture.drain_to_newest = config.boolean("wgc_drain_to_newest", false);
            // The pool's textures can be shared with D3D12 on supported AMD
            // drivers. The same fenced handoff used by DDX keeps the copy and
            // AMF conversion off a busy graphics queue. Keep an independent
            // switch for comparisons and drivers that need the D3D11 path.
            if config.boolean("wgc_compute_copy", true)
                && crate::compute::enabled(config)
                && crate::compute::copies_on(&capture.gpu.device)
            {
                match crate::compute::Compute::for_device(&capture.gpu.device)
                    .and_then(|compute| crate::compute::Handoff::new(compute, &capture.gpu))
                {
                    Ok(compute) => capture.owned.compute = Some(compute),
                    Err(error) => {
                        warnings.set("capture_compute", format!("WGC compute copies unavailable ({error:#}); copying on the graphics queue. Lower game GPU load or update the AMD driver if capture stutters."))
                    }
                }
            }
            Ok(capture)
        };
        match kind {
            "wgc" => Ok(Self::Wgc(Box::new(wgc(gpu)?))),
            "ddx" | "dxgi" => Ok(Self::Dxgi(Box::new(duplication(gpu)?))),
            _ => open_stream_capture("wgc", &warnings, |backend| {
                if backend == "wgc" {
                    wgc(gpu.clone()).map(|capture| Self::Wgc(Box::new(capture)))
                } else {
                    duplication(gpu.clone()).map(|d| Self::Dxgi(Box::new(d)))
                }
            }),
        }
    }
    pub fn next_frame(&mut self) -> Result<Option<Image>> {
        match self {
            Self::Wgc(w) => w.next_frame(),
            Self::WgcWorker(w) => w.next_frame(),
            Self::Dxgi(d) => d.next(Duration::from_millis(1)),
            Self::Closed => bail!("capture is closed"),
        }
    }
    pub fn next_gpu(&mut self) -> Result<Option<GpuImage>> {
        let result = match self {
            Self::Wgc(w) => w.next_gpu(),
            Self::WgcWorker(w) => w.next_gpu(),
            Self::Dxgi(d) => d.next_gpu(),
            Self::Closed => bail!("capture is closed"),
        };
        result.map_err(|error| match self.device_removed() {
            Some(loss) => error.context(loss),
            None => error,
        })
    }
    pub fn device_removed(&self) -> Option<crate::device_loss::DeviceLost> {
        let gpu = match self {
            Self::Wgc(w) => &w.gpu,
            Self::WgcWorker(w) => &w.gpu,
            Self::Dxgi(d) => &d.gpu,
            Self::Closed => return None,
        };
        crate::device_loss::DeviceLost::d3d11(&gpu.device)
    }
}

#[cfg(test)]
mod tests;
