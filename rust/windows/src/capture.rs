use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::time::{Duration, Instant};
use windows::{
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
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
pub struct Device {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub output: IDXGIOutput1,
    pub display: Display,
}
impl Device {
    pub fn new(name: &str) -> Result<Self> {
        unsafe {
            let choices = displays()?;
            let display = choices
                .iter()
                .find(|d| d.display_name == name || d.device_id == name)
                .or_else(|| choices.iter().find(|d| d.primary))
                .or_else(|| choices.first())
                .context("no desktop display")?
                .clone();
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let adapter = factory.EnumAdapters1(display.adapter_index)?;
            let output = adapter.EnumOutputs(display.output_index)?.cast()?;
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
            Ok(Self {
                device: device.unwrap(),
                context: context.unwrap(),
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
}
impl Duplication {
    pub fn new(name: &str) -> Result<Self> {
        Self::new_format(name, false)
    }
    pub fn new_format(name: &str, hdr: bool) -> Result<Self> {
        let gpu = Device::new(name)?;
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
pub struct Wgc {
    gpu: Device,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    staging: Option<ID3D11Texture2D>,
}
impl Wgc {
    pub fn new(name: &str) -> Result<Self> {
        Self::new_format(name, false)
    }
    pub fn new_format(name: &str, hdr: bool) -> Result<Self> {
        unsafe {
            let gpu = Device::new(name)?;
            let d = gpu.output.GetDesc()?;
            let interop: IGraphicsCaptureItemInterop =
                windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            let item: GraphicsCaptureItem = interop.CreateForMonitor(d.Monitor)?;
            let dxgi: IDXGIDevice = gpu.device.cast()?;
            let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                &winrt,
                if hdr {
                    DirectXPixelFormat::R16G16B16A16Float
                } else {
                    DirectXPixelFormat::B8G8R8A8UIntNormalized
                },
                2,
                item.Size()?,
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
            })
        }
    }
    pub fn next_frame(&mut self) -> Result<Option<Image>> {
        let frame = match self.pool.TryGetNextFrame() {
            Ok(f) => f,
            Err(_) => return Ok(None),
        };
        let result = (|| -> Result<Image> {
            let surface = frame.Surface()?;
            let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
            let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
            read_texture(&self.gpu, &texture, &mut self.staging)
        })();
        frame.Close()?;
        result.map(Some)
    }
}
impl Drop for Wgc {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
pub enum Capture {
    Wgc(Wgc),
    Dxgi(Duplication),
}
impl Capture {
    pub fn new(name: &str, kind: &str) -> Result<Self> {
        Self::new_format(name, kind, false)
    }
    pub fn new_format(name: &str, kind: &str, hdr: bool) -> Result<Self> {
        match kind {
            "wgc" => Ok(Self::Wgc(Wgc::new_format(name, hdr)?)),
            "dxgi" => Ok(Self::Dxgi(Duplication::new_format(name, hdr)?)),
            _ => Wgc::new_format(name, hdr)
                .map(Self::Wgc)
                .or_else(|_| Duplication::new_format(name, hdr).map(Self::Dxgi)),
        }
    }
    pub fn next_frame(&mut self) -> Result<Option<Image>> {
        match self {
            Self::Wgc(w) => w.next_frame(),
            Self::Dxgi(d) => d.next(Duration::from_millis(1)),
        }
    }
}
