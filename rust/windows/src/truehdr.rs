//! Optional Rust NGX adapter; creation, conversion and teardown stay on one thread.
#![warn(clippy::undocumented_unsafe_blocks)]

use crate::capture::{Device, GpuImage, GpuPool, Image, Pixel, read_texture};
use anyhow::{Context, Result, bail};
use std::{ffi::c_void, ptr};
use windows::{
    Win32::Graphics::{Direct3D11::*, Dxgi::Common::*},
    core::Interface,
};
type Create = unsafe extern "C" fn(*mut c_void, *mut u32) -> *mut c_void;
type Convert =
    unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32, u32, u32, *mut u32) -> *mut c_void;
type Destroy = unsafe extern "C" fn(*mut c_void);
pub struct Filter {
    state: *mut c_void,
    convert: Convert,
    destroy: Destroy,
    device: Device,
    input: Option<ID3D11Texture2D>,
    staging: Option<ID3D11Texture2D>,
    size: (u32, u32),
    parameters: [u32; 4],
    pool: GpuPool,
    _dll: libloading::Library,
}
pub fn available() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("butterpollo_truehdr.dll")))
        .is_some_and(|p| p.exists())
}
impl Filter {
    pub fn set_parameters(&mut self, parameters: [u32; 4]) {
        self.parameters = parameters;
    }
    pub fn new(display: &str, parameters: [u32; 4]) -> Result<Self> {
        Self::new_device(Device::new(display)?, parameters)
    }
    pub fn new_gpu(image: &GpuImage, parameters: [u32; 4]) -> Result<Self> {
        Self::new_device(image.gpu.clone(), parameters)
    }
    fn new_device(device: Device, parameters: [u32; 4]) -> Result<Self> {
        // SAFETY: `device.device` is a live D3D11 device; the DXGI calls only read its adapter.
        unsafe {
            let adapter = device
                .device
                .cast::<windows::Win32::Graphics::Dxgi::IDXGIDevice>()?
                .GetAdapter()?
                .GetDesc()?;
            if adapter.VendorId != 0x10de {
                bail!("TrueHDR requires an NVIDIA adapter");
            }
        }
        let path = std::env::current_exe()?
            .parent()
            .context("executable directory unavailable")?
            .join("butterpollo_truehdr.dll");
        // SAFETY: The DLL beside the executable exports these symbols with these types (ABI 1 is
        // checked first), and Filter keeps `dll` and `device` while the pointers are used.
        unsafe {
            let dll =
                libloading::Library::new(path).context("Rust TrueHDR runtime is unavailable")?;
            let abi = dll.get::<unsafe extern "C" fn() -> u32>(b"butterpollo_truehdr_abi\0")?;
            if abi() != 1 {
                bail!("incompatible Rust TrueHDR runtime ABI");
            }
            let create = *dll.get::<Create>(b"butterpollo_truehdr_create\0")?;
            let convert = *dll.get::<Convert>(b"butterpollo_truehdr_convert\0")?;
            let destroy = *dll.get::<Destroy>(b"butterpollo_truehdr_destroy\0")?;
            let mut error = 0;
            let state = create(device.device.as_raw(), &mut error);
            if state.is_null() {
                bail!("NVIDIA TrueHDR initialization failed ({error:#x})");
            }
            Ok(Self {
                state,
                convert,
                destroy,
                device,
                input: None,
                staging: None,
                size: (0, 0),
                parameters,
                pool: GpuPool::default(),
                _dll: dll,
            })
        }
    }
    /// Snapshot NGX's mutable output into the bounded pool before another conversion.
    pub fn apply_gpu(&mut self, image: &GpuImage) -> Result<GpuImage> {
        if image.pixel != Pixel::Bgra8 || image.gpu.device.as_raw() != self.device.device.as_raw() {
            bail!("TrueHDR requires an SDR texture on the capture device");
        }
        // SAFETY: `state` is the live NGX state, created on this device, whose multithread lock is
        // held; the returned texture is only borrowed while it is copied into the pool.
        unsafe {
            let lock: ID3D11Multithread = self.device.context.cast()?;
            lock.Enter();
            let result = (|| {
                let [contrast, saturation, middle, peak] = self.parameters;
                let mut error = 0;
                let output = (self.convert)(
                    self.state,
                    image.texture.as_raw(),
                    contrast,
                    saturation,
                    middle,
                    peak,
                    &mut error,
                );
                let texture = ID3D11Texture2D::from_raw_borrowed(&output)
                    .context(format!("NVIDIA TrueHDR conversion failed ({error:#x})"))?;
                let mut converted = self
                    .pool
                    .copy(&self.device, texture)?
                    .context("TrueHDR texture pool is occupied by pending frames")?;
                converted.captured = image.captured;
                Ok(converted)
            })();
            lock.Leave();
            result
        }
    }
    pub fn apply(&mut self, image: &Image) -> Result<Image> {
        if image.pixel != Pixel::Bgra8 {
            bail!("TrueHDR requires an SDR capture surface");
        }
        if image.bytes.len() < image.stride * image.height as usize
            || image.stride < image.width as usize * 4
        {
            bail!("invalid TrueHDR input image");
        }
        // SAFETY: `image.bytes` holds stride * height bytes (checked above) for UpdateSubresource,
        // and `state` is the live NGX state; the output is only borrowed while it is read back.
        unsafe {
            if self.size != (image.width, image.height) {
                self.input = None;
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: image.width,
                    Height: image.height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    ..Default::default()
                };
                self.device
                    .device
                    .CreateTexture2D(&desc, None, Some(&mut self.input))?;
                self.size = (image.width, image.height);
            }
            let input = self.input.as_ref().unwrap();
            self.device.context.UpdateSubresource(
                input,
                0,
                None,
                image.bytes.as_ptr().cast(),
                image.stride as u32,
                0,
            );
            let [contrast, saturation, middle, peak] = self.parameters;
            let mut error = 0;
            let output = (self.convert)(
                self.state,
                input.as_raw(),
                contrast,
                saturation,
                middle,
                peak,
                &mut error,
            );
            let texture = ID3D11Texture2D::from_raw_borrowed(&output)
                .context(format!("NVIDIA TrueHDR conversion failed ({error:#x})"))?;
            read_texture(&self.device, texture, &mut self.staging)
        }
    }
}
impl Drop for Filter {
    fn drop(&mut self) {
        // SAFETY: `state` came from create and is destroyed once, here; `_dll` is dropped after
        // this.
        unsafe {
            (self.destroy)(self.state);
        }
        self.state = ptr::null_mut();
    }
}
