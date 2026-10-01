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
    unsafe {
        // A failed call means the embedding process already set its DPI context.
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
}
fn wide(b: &[u16]) -> String {
    String::from_utf16_lossy(&b[..b.iter().position(|v| *v == 0).unwrap_or(b.len())])
}
pub fn displays() -> Result<Vec<Display>> {
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
                let name = wide(&d.DeviceName);
                let r = d.DesktopCoordinates;
                result.push(Display {
                    device_id: name.clone(),
                    display_name: name,
                    friendly_name: wide(&ad.Description),
                    width: (r.right - r.left) as u32,
                    height: (r.bottom - r.top) as u32,
                    x: r.left,
                    y: r.top,
                    primary: r.left == 0 && r.top == 0,
                    adapter: wide(&ad.Description),
                    adapter_index: a,
                    output_index: o,
                });
            }
        }
        Ok(result)
    }
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
    if unsafe { DisplayConfigGetDeviceInfo(&mut info.header) } != 0 {
        return None;
    }
    let path = wide(&info.adapterDevicePath);
    let path = path.strip_prefix(r"\\?\")?;
    let mut parts = path.split('#');
    let (bus, hardware, instance) = (parts.next()?, parts.next()?, parts.next()?);
    if bus.is_empty() || hardware.is_empty() || instance.is_empty() {
        return None;
    }
    Some(format!("{bus}\\{hardware}\\{instance}"))
}
pub fn gpus() -> Result<Vec<Gpu>> {
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
                    name: wide(&info.Description),
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
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for ComGuard {
    fn drop(&mut self) {
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
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
            Self { handle }
        }
    }
}
impl Drop for Priority {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
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
    pub output: IDXGIOutput1,
    pub display: Display,
}
impl Device {
    /// Read the selected output's luminance; conversion produces Rec.2020/D65.
    pub fn hdr_metadata(&self) -> butterpollo_core::hdr::Metadata {
        unsafe {
            self.output
                .cast::<IDXGIOutput6>()
                .and_then(|output| output.GetDesc1())
        }
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
    pub fn new_adapter(name: &str, adapter_name: &str, pnp_id: &str) -> Result<Self> {
        unsafe {
            let choices = displays()?;
            let mut display = choices
                .iter()
                .find(|d| d.display_name == name || d.device_id == name)
                .or_else(|| choices.iter().find(|d| d.primary))
                .or_else(|| choices.first())
                .context("no desktop display")?
                .clone();
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let adapter = factory.EnumAdapters1(display.adapter_index)?;
            let output = adapter.EnumOutputs(display.output_index)?.cast()?;
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
                        wide(&desc.Description) == adapter_name
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
            display.adapter = wide(&adapter.GetDesc1()?.Description);
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                &adapter,
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
}
impl Duplication {
    pub fn new(name: &str) -> Result<Self> {
        Self::new_format(name, false)
    }
    pub fn new_format(name: &str, hdr: bool) -> Result<Self> {
        Self::new_device(Device::new(name)?, hdr)
    }
    fn new_device(gpu: Device, hdr: bool) -> Result<Self> {
        let duplicate = unsafe {
            if hdr {
                gpu.output.cast::<IDXGIOutput5>()?.DuplicateOutput1(
                    &gpu.device,
                    0,
                    &[
                        DXGI_FORMAT_R16G16B16A16_FLOAT,
                        DXGI_FORMAT_R10G10B10A2_UNORM,
                    ],
                )?
            } else {
                gpu.output.DuplicateOutput(&gpu.device)?
            }
        };
        Ok(Self {
            gpu,
            duplicate,
            staging: None,
            owned: GpuPool::default(),
        })
    }
    pub fn next(&mut self, timeout: Duration) -> Result<Option<Image>> {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource = None;
            match self.duplicate.AcquireNextFrame(
                timeout.as_millis().min(100) as u32,
                &mut info,
                &mut resource,
            ) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
                Err(e) => return Err(e.into()),
            }
            let result = (|| -> Result<Image> {
                let texture: ID3D11Texture2D =
                    resource.context("empty captured texture")?.cast()?;
                read_texture(&self.gpu, &texture, &mut self.staging)
            })();
            let _ = self.duplicate.ReleaseFrame();
            result.map(Some)
        }
    }
    pub fn next_gpu(&mut self) -> Result<Option<GpuImage>> {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource = None;
            match self.duplicate.AcquireNextFrame(1, &mut info, &mut resource) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
                Err(e) => return Err(e.into()),
            }
            let result = (|| {
                let texture: ID3D11Texture2D =
                    resource.context("empty captured texture")?.cast()?;
                self.owned.copy(&self.gpu, &texture)
            })();
            let _ = self.duplicate.ReleaseFrame();
            result
        }
    }
}

/// An immutable, owned GPU snapshot. The capture pool never overwrites a
/// texture until the latest-frame slot and every encoder have released it.
#[derive(Clone)]
pub struct GpuImage {
    pub width: u32,
    pub height: u32,
    pub pixel: Pixel,
    pub captured: Instant,
    pub gpu: Device,
    pub texture: std::sync::Arc<ID3D11Texture2D>,
}
impl GpuImage {
    pub fn readback(&self, staging: &mut Option<ID3D11Texture2D>) -> Result<Image> {
        read_texture(&self.gpu, &self.texture, staging)
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
                width: image.width,
                height: image.height,
                pixel: image.pixel,
                captured: image.captured,
                gpu: gpu.clone(),
                texture: std::sync::Arc::new(texture.unwrap()),
            })
        }
    }
}

#[derive(Default)]
pub(crate) struct GpuPool {
    textures: Vec<std::sync::Arc<ID3D11Texture2D>>,
}
impl GpuPool {
    pub(crate) fn copy(
        &mut self,
        gpu: &Device,
        source: &ID3D11Texture2D,
    ) -> Result<Option<GpuImage>> {
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
                desc.MiscFlags = 0;
                let mut texture = None;
                gpu.device
                    .CreateTexture2D(&desc, None, Some(&mut texture))?;
                let texture = std::sync::Arc::new(texture.unwrap());
                self.textures.push(texture.clone());
                texture
            };
            gpu.context.CopyResource(texture.as_ref(), source);
            Ok(Some(GpuImage {
                width: desc.Width,
                height: desc.Height,
                pixel,
                captured: Instant::now(),
                gpu: gpu.clone(),
                texture,
            }))
        }
    }
}
pub fn read_texture(
    gpu: &Device,
    texture: &ID3D11Texture2D,
    staging: &mut Option<ID3D11Texture2D>,
) -> Result<Image> {
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
pub struct Wgc {
    gpu: Device,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    staging: Option<ID3D11Texture2D>,
    owned: GpuPool,
    grid: Option<std::sync::Arc<std::sync::Mutex<ClaimGrid>>>,
    intervals: butterpollo_core::capture_policy::Intervals,
    origin: Instant,
    last_publish: Instant,
    held: Option<(Direct3D11CaptureFrame, Instant, Instant)>,
    size: (i32, i32),
    color_space: Option<DXGI_COLOR_SPACE_TYPE>,
    color_check: Instant,
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
        Self::new_device(Device::new(name)?, hdr)
    }
    fn new_device(gpu: Device, hdr: bool) -> Result<Self> {
        unsafe {
            let d = gpu.output.GetDesc()?;
            let color_space = gpu
                .output
                .cast::<IDXGIOutput6>()
                .and_then(|output| output.GetDesc1())
                .map(|desc| desc.ColorSpace)
                .ok();
            let native_hdr = color_space
                .map(|space| space == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)
                .unwrap_or(hdr);
            let interop: IGraphicsCaptureItemInterop =
                windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            pin_capture_runtime()?;
            let item: GraphicsCaptureItem = interop.CreateForMonitor(d.Monitor)?;
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
                2,
                size,
            )?;
            let session = pool.CreateCaptureSession(&item)?;
            session.SetIsCursorCaptureEnabled(true)?;
            let _ = session.SetIsBorderRequired(false);
            session.StartCapture()?;
            Ok(Self {
                gpu,
                pool,
                session,
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
            })
        }
    }
    pub fn next_frame(&mut self) -> Result<Option<Image>> {
        self.check_color_space()?;
        let Some(frame) = self.try_frame()? else {
            return Ok(None);
        };
        let result = (|| -> Result<Image> {
            self.check_size(&frame)?;
            let surface = frame.Surface()?;
            let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
            let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
            read_texture(&self.gpu, &texture, &mut self.staging)
        })();
        frame.Close()?;
        result.map(Some)
    }
    pub fn next_gpu(&mut self) -> Result<Option<GpuImage>> {
        self.check_color_space()?;
        let now = Instant::now();
        if let Some(frame) = self.try_frame()? {
            if let Err(error) = self.check_size(&frame) {
                let _ = frame.Close();
                return Err(error);
            }
            let composition = self.intervals.observe(nanos(now, self.origin));
            if let Some((previous, _, _)) = self.held.take() {
                previous.Close()?;
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
            self.held = Some((frame, deadline.unwrap_or(now), now));
        }
        if self
            .held
            .as_ref()
            .is_none_or(|(_, deadline, _)| *deadline > now)
        {
            return Ok(None);
        }
        let (frame, _, captured) = self.held.take().unwrap();
        let result = (|| {
            let surface = frame.Surface()?;
            let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
            let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
            let mut image = self.owned.copy(&self.gpu, &texture)?;
            if let Some(image) = image.as_mut() {
                image.captured = captured;
            }
            Ok(image)
        })();
        frame.Close()?;
        self.last_publish = now;
        result
    }
    fn try_frame(&self) -> Result<Option<Direct3D11CaptureFrame>> {
        // An empty pool returns a successful HRESULT and a null interface. The
        // generated binding requires a non-null frame, so preserve the HRESULT
        // and optional output separately instead of swallowing every error.
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
        let current = unsafe {
            self.gpu
                .output
                .cast::<IDXGIOutput6>()
                .and_then(|output| output.GetDesc1())
        }
        .map(|desc| desc.ColorSpace)
        .ok();
        if current.is_some() && current != self.color_space {
            bail!("capture output color space changed");
        }
        Ok(())
    }
}

impl Drop for Wgc {
    fn drop(&mut self) {
        if let Some((frame, _, _)) = self.held.take() {
            let _ = frame.Close();
        }
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
pub enum Capture {
    Wgc(Box<Wgc>),
    Dxgi(Duplication),
}
impl Capture {
    pub fn set_claim_grid(
        &mut self,
        grid: std::sync::Arc<std::sync::Mutex<ClaimGrid>>,
        enabled: bool,
    ) {
        if let Self::Wgc(wgc) = self {
            wgc.grid = enabled.then_some(grid);
        }
    }
    pub fn publication_deadline(&self) -> Option<Instant> {
        match self {
            Self::Wgc(wgc) => wgc.held.as_ref().map(|(_, deadline, _)| *deadline),
            _ => None,
        }
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
        let gpu = Device::new_adapter(
            name,
            config.get("adapter_name", ""),
            config.get("adapter_pnp_id", ""),
        )?;
        if let Err(error) = crate::gpu_priority::configure(&gpu, config) {
            tracing::debug!(%error, "GPU priority remains at the process default");
        }
        match kind {
            "wgc" => Ok(Self::Wgc(Box::new(Wgc::new_device(gpu, hdr)?))),
            "dxgi" => Ok(Self::Dxgi(Duplication::new_device(gpu, hdr)?)),
            _ => Wgc::new_device(gpu.clone(), hdr)
                .map(|capture| Self::Wgc(Box::new(capture)))
                .or_else(|_| Duplication::new_device(gpu, hdr).map(Self::Dxgi)),
        }
    }
    pub fn next_frame(&mut self) -> Result<Option<Image>> {
        match self {
            Self::Wgc(w) => w.next_frame(),
            Self::Dxgi(d) => d.next(Duration::from_millis(1)),
        }
    }
    pub fn next_gpu(&mut self) -> Result<Option<GpuImage>> {
        match self {
            Self::Wgc(w) => w.next_gpu(),
            Self::Dxgi(d) => d.next_gpu(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an interactive Windows desktop with WGC support"]
    fn wgc_reconnect_and_com_teardown_keep_the_runtime_loaded() -> Result<()> {
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        for cycle in 0..16 {
            std::thread::spawn(move || -> Result<()> {
                let com = ComGuard::new()?;
                let mut capture = Wgc::new("")?;
                let until = Instant::now() + Duration::from_secs(5);
                loop {
                    if let Some(frame) = capture.next_gpu()? {
                        assert!(frame.width > 0 && frame.height > 0);
                        break;
                    }
                    anyhow::ensure!(
                        Instant::now() < until,
                        "WGC reconnect {cycle} produced no frame"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                capture.pool.Close()?;
                assert!(
                    capture.next_gpu().is_err(),
                    "a closed pool must trigger GPU capture recovery"
                );
                assert!(
                    capture.next_frame().is_err(),
                    "a closed pool must trigger CPU capture recovery"
                );
                drop(capture);
                drop(com);
                // Reproduce the unload boundary before a subsequent stream/thread.
                unsafe {
                    CoFreeUnusedLibrariesEx(0, None);
                    GetModuleHandleW(windows::core::w!("GraphicsCapture.dll"))?;
                }
                Ok(())
            })
            .join()
            .expect("capture worker panicked")?;
        }
        Ok(())
    }
}
