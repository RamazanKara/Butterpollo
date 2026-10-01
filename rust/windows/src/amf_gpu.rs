//! Wrap the Rust D3D11 converter's GPU texture in AMF's native surface ABI.
use crate::{
    amf::check,
    amf_abi::*,
    capture::{Device, GpuImage, Pixel},
};
use anyhow::{Context, Result};
use std::{ptr, sync::Arc};
use windows::{Win32::Graphics::Direct3D11::ID3D11Texture2D, core::Interface};
pub(crate) fn boolean(value: bool) -> AMFVariantStruct {
    AMFVariantStruct {
        type_: AMF_VARIANT_TYPE_AMF_VARIANT_BOOL,
        __bindgen_anon_1: AMFVariantStruct__bindgen_ty_1 {
            boolValue: u8::from(value),
        },
    }
}
pub(crate) struct Surface(pub *mut AMFSurface);
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            ((*(*self.0).pVtbl).Release.unwrap())(self.0);
        }
    }
}
pub(crate) struct Converter {
    color: crate::gpu_color::Converter,
    pub source: (u32, u32, Pixel),
}
impl Converter {
    pub fn new(
        gpu: &Device,
        config: &butterpollo_core::rtsp::Negotiated,
        source: (u32, u32, Pixel),
    ) -> Result<Self> {
        Ok(Self {
            color: crate::gpu_color::Converter::new(gpu, config, source)?,
            source,
        })
    }
    pub fn convert(
        &mut self,
        context: *mut AMFContext,
        image: &GpuImage,
    ) -> Result<(Surface, Arc<ID3D11Texture2D>)> {
        let texture = self.color.convert(image)?;
        unsafe {
            let mut surface = ptr::null_mut();
            check(((*(*context).pVtbl).CreateSurfaceFromDX11Native.unwrap())(
                context,
                texture.as_raw(),
                &mut surface,
                ptr::null_mut(),
            ))
            .context("AMF wrap native YUV texture")?;
            Ok((Surface(surface), texture))
        }
    }
}
