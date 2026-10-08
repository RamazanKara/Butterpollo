//! Wrap the Rust D3D11 converter's GPU texture in AMF's native surface ABI.
#![warn(clippy::undocumented_unsafe_blocks)]

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
        // SAFETY: `self.0` is a non-null surface from a CreateSurfaceFrom* call in this file, and
        // this `Surface` owns the one reference it returned.
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
    textures: Mutex<BTreeMap<usize, Box<dyn std::any::Any + Send>>>,
}
static OBSERVER: AMFSurfaceObserverVtbl = AMFSurfaceObserverVtbl {
    OnSurfaceDataRelease: Some(released),
};
unsafe extern "C" fn released(observer: *mut AMFSurfaceObserver, surface: *mut AMFSurface) {
    if observer.is_null() {
        return;
    }
    // SAFETY: AMF passes back the observer from `Ownership::raw`, the first field of a boxed
    // #[repr(C)] Ownership that the encoder keeps until AMF is terminated.
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
    /// Keep `owner` alive until AMF releases `surface`.
    pub(crate) fn hold(&self, surface: *mut AMFSurface, owner: Box<dyn std::any::Any + Send>) {
        self.textures
            .lock()
            .unwrap()
            .insert(surface as usize, owner);
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
        wrap(context, &texture, ownership)
    }
}
/// `texture` as an AMF input surface, kept alive until AMF releases it.
fn wrap(
    context: *mut AMFContext,
    texture: &Arc<ID3D11Texture2D>,
    ownership: &mut Ownership,
) -> Result<(Surface, Arc<ID3D11Texture2D>)> {
    // SAFETY: `context` is a live AMF context, `ownership` is boxed so the observer address stays
    // valid, and `texture` is held in it until AMF releases the surface.
    unsafe {
        let mut surface = ptr::null_mut();
        check(((*(*context).pVtbl).CreateSurfaceFromDX11Native.unwrap())(
            context,
            texture.as_raw(),
            &mut surface,
            ownership.raw(),
        ))
        .context("AMF wrap native texture")?;
        if surface.is_null() {
            anyhow::bail!("AMF returned no native surface");
        }
        ownership.hold(surface, Box::new(texture.clone()));
        Ok((Surface(surface), texture.clone()))
    }
}
/// AMF's context interface with D3D12 (`AMFContext2`). Only the D3D12
/// entries are named; the rest of the table is AMFContext1's.
#[repr(C)]
struct Context2Vtbl {
    _context1: [usize; 65],
    init_dx12:
        unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, i32) -> AMF_RESULT,
    _get_dx12_device: usize,
    _lock_dx12: usize,
    _unlock_dx12: usize,
    surface_from_dx12: unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *mut std::ffi::c_void,
        *mut *mut AMFSurface,
        *mut AMFSurfaceObserver,
    ) -> AMF_RESULT,
}
#[repr(C)]
struct Context2 {
    vtable: *const Context2Vtbl,
}
/// AMFContext2's interface ID.
const CONTEXT2: AMFGuid = AMFGuid {
    data1: 0x726241d3,
    data2: 0xbd46,
    data3: 0x4e90,
    data41: 0x99,
    data42: 0x68,
    data43: 0x93,
    data44: 0xe0,
    data45: 0x7e,
    data46: 0xa2,
    data47: 0x98,
    data48: 0x4d,
};
/// The context's D3D12 interface.
pub(crate) struct D3d12Context(*mut Context2);
impl D3d12Context {
    pub fn new(context: *mut AMFContext) -> Result<Self> {
        // SAFETY: `context` is a live AMF context, and QueryInterface writes an added reference to
        // an AMFContext2 into `interface`, which is checked for null.
        unsafe {
            let mut interface = ptr::null_mut();
            check(((*(*context).pVtbl).QueryInterface.unwrap())(
                context,
                &CONTEXT2,
                &mut interface,
            ))
            .context("AMF without D3D12 support")?;
            if interface.is_null() {
                anyhow::bail!("AMF returned no D3D12 context");
            }
            Ok(Self(interface.cast()))
        }
    }
    pub fn init(&self, device: &windows::Win32::Graphics::Direct3D12::ID3D12Device) -> Result<()> {
        // SAFETY: `self.0` is the live AMFContext2 from `new`, whose table has InitDX12 right after
        // AMFContext1's 65 entries, and `device` is a live D3D12 device.
        unsafe {
            check(((*(*self.0).vtable).init_dx12)(
                self.0.cast(),
                device.as_raw(),
                120,
            ))
            .context("AMF InitDX12")
        }
    }
    /// `texture` as an AMF input surface, kept alive until AMF releases it.
    pub fn wrap(
        &self,
        texture: &Arc<windows::Win32::Graphics::Direct3D12::ID3D12Resource>,
        ownership: &mut Ownership,
    ) -> Result<Surface> {
        // SAFETY: `self.0` is the live AMFContext2, `ownership` is boxed so the observer address
        // stays valid, and `texture` is held in it until AMF releases the surface.
        unsafe {
            let mut surface = ptr::null_mut();
            check(((*(*self.0).vtable).surface_from_dx12)(
                self.0.cast(),
                texture.as_raw(),
                &mut surface,
                ownership.raw(),
            ))
            .context("AMF wrap D3D12 texture")?;
            if surface.is_null() {
                anyhow::bail!("AMF returned no D3D12 surface");
            }
            ownership.hold(surface, Box::new(texture.clone()));
            Ok(Surface(surface))
        }
    }
}
impl Drop for D3d12Context {
    fn drop(&mut self) {
        // SAFETY: `self.0` holds the one reference QueryInterface added in `new`, and entry 1 of
        // AMFContext1's table is Release.
        unsafe {
            let release: unsafe extern "C" fn(*mut std::ffi::c_void) -> i64 =
                std::mem::transmute((*(*self.0).vtable)._context1[1]);
            release(self.0.cast());
        }
    }
}
// AMF's D3D12 synchronisation private data (core/D3D12AMF.h).
const RESOURCE_STATE: windows::core::GUID = windows::core::GUID::from_values(
    0x452da9bf,
    0x4ad7,
    0x47a5,
    [0xa6, 0x9b, 0x96, 0xd3, 0x23, 0x76, 0xf2, 0xf3],
);
const FENCE: windows::core::GUID = windows::core::GUID::from_values(
    0x910a7928,
    0x57bd,
    0x4b04,
    [0x91, 0xa3, 0xe7, 0xb8, 0x04, 0x12, 0xcd, 0xa5],
);
const FENCE_VALUE: windows::core::GUID = windows::core::GUID::from_values(
    0x62a693d3,
    0xbb4a,
    0x46c9,
    [0xa5, 0x04, 0x9a, 0x8e, 0x97, 0xbf, 0xf0, 0x56],
);
/// Tell AMF to wait for `fence` to reach `value` before reading `texture`
/// (in the COMMON state). AMF signals the fence again when it has read the
/// texture and records the new value on the fence, so the fence must be the
/// texture's alone.
pub(crate) fn synchronize(
    texture: &windows::Win32::Graphics::Direct3D12::ID3D12Resource,
    fence: &windows::Win32::Graphics::Direct3D12::ID3D12Fence,
    value: u64,
) -> Result<()> {
    // SAFETY: `texture` and `fence` are live D3D12 objects, and each data pointer refers to a local
    // of exactly the size passed.
    unsafe {
        let state: u32 = windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATE_COMMON.0 as u32;
        texture.SetPrivateData(&RESOURCE_STATE, 4, Some((&state as *const u32).cast()))?;
        texture.SetPrivateDataInterface(&FENCE, fence)?;
        fence.SetPrivateData(&FENCE_VALUE, 8, Some((&value as *const u64).cast()))?;
    }
    Ok(())
}
/// The value last recorded on `fence` for AMF, by us or by AMF.
pub(crate) fn fence_value(
    fence: &windows::Win32::Graphics::Direct3D12::ID3D12Fence,
) -> Option<u64> {
    let mut value = 0u64;
    let mut size = 8u32;
    // SAFETY: `fence` is live, and `value` is a local u64 whose 8-byte size is passed in `size`.
    unsafe {
        fence
            .GetPrivateData(
                &FENCE_VALUE,
                &mut size,
                Some((&mut value as *mut u64).cast()),
            )
            .ok()?;
    }
    (size == 8).then_some(value)
}
