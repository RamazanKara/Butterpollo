//! Wrap the Rust D3D11 converter's GPU texture in AMF's native surface ABI.
use crate::{
    amf::check,
    amf_abi::*,
    capture::{Device, GpuImage, Pixel},
};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    ptr,
    sync::{Arc, Mutex},
};
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
/// AMF can retain an input after emitting its output PTS. Keep the pool's Arc
/// until its native release callback, using one stable observer for the whole
/// encoder lifetime. Duplicate callbacks are harmless.
#[repr(C)]
pub(crate) struct Ownership {
    observer: AMFSurfaceObserver,
    textures: Mutex<BTreeMap<usize, Arc<ID3D11Texture2D>>>,
}
static OBSERVER: AMFSurfaceObserverVtbl = AMFSurfaceObserverVtbl {
    OnSurfaceDataRelease: Some(released),
};
unsafe extern "C" fn released(observer: *mut AMFSurfaceObserver, surface: *mut AMFSurface) {
    if observer.is_null() {
        return;
    }
    let ownership = unsafe { &*observer.cast::<Ownership>() };
    if let Ok(mut textures) = ownership.textures.lock() {
        textures.remove(&(surface as usize));
    }
}
impl Ownership {
    pub fn new() -> Box<Self> {
        Box::new(Self {
            observer: AMFSurfaceObserver { pVtbl: &OBSERVER },
            textures: Default::default(),
        })
    }
    pub fn retained(&self) -> usize {
        self.textures.lock().unwrap().len()
    }
    fn raw(&mut self) -> *mut AMFSurfaceObserver {
        &mut self.observer
    }
}
pub(crate) struct Converter {
    pub color: crate::gpu_color::Converter,
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
        ownership: &mut Ownership,
    ) -> Result<(Surface, Arc<ID3D11Texture2D>)> {
        let texture = self.color.convert(image)?;
        unsafe {
            let mut surface = ptr::null_mut();
            check(((*(*context).pVtbl).CreateSurfaceFromDX11Native.unwrap())(
                context,
                texture.as_raw(),
                &mut surface,
                ownership.raw(),
            ))
            .context("AMF wrap native YUV texture")?;
            if surface.is_null() {
                anyhow::bail!("AMF returned no native surface");
            }
            ownership
                .textures
                .lock()
                .unwrap()
                .insert(surface as usize, texture.clone());
            Ok((Surface(surface), texture))
        }
    }
}
