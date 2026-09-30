//! Optional Rust NGX adapter; creation, conversion and teardown stay on one thread.
use crate::capture::{Device, Image, Pixel, read_texture};
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
    _dll: libloading::Library,
}
pub fn available() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("butterpollo_truehdr.dll")))
        .is_some_and(|p| p.exists())
}
impl Filter {
    pub fn new(display: &str, parameters: [u32; 4]) -> Result<Self> {
        let device = Device::new(display)?;
        let path = std::env::current_exe()?
            .parent()
            .context("executable directory unavailable")?
            .join("butterpollo_truehdr.dll");
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
                _dll: dll,
            })
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
        unsafe {
            (self.destroy)(self.state);
        }
        self.state = ptr::null_mut();
    }
}
