//! MSVC Rust DLL adapting NVIDIA's vendor C ABI to the GNU Rust host.
//! No C++ adapter is compiled. ABI definitions are from RTX Video SDK 1.1.0.
use std::{
    collections::BTreeMap,
    ffi::{c_char, c_void},
    ptr,
    sync::Mutex,
};
use windows::{
    Win32::Graphics::{
        Direct3D11::*,
        Dxgi::{Common::*, IDXGIDevice},
    },
    core::Interface,
};
unsafe extern "C" {
    fn NVSDK_NGX_D3D11_Init(app: u64, path: *const u16, device: *mut c_void, version: u32) -> u32;
    fn NVSDK_NGX_D3D11_Shutdown1(device: *mut c_void) -> u32;
    fn NVSDK_NGX_D3D11_GetCapabilityParameters(params: *mut *mut c_void) -> u32;
    fn NVSDK_NGX_D3D11_DestroyParameters(params: *mut c_void) -> u32;
    fn NVSDK_NGX_D3D11_CreateFeature(
        context: *mut c_void,
        feature: u32,
        params: *const c_void,
        handle: *mut *mut c_void,
    ) -> u32;
    fn NVSDK_NGX_D3D11_ReleaseFeature(handle: *mut c_void) -> u32;
    fn NVSDK_NGX_D3D11_EvaluateFeature_C(
        context: *mut c_void,
        handle: *const c_void,
        params: *const c_void,
        callback: *const c_void,
    ) -> u32;
    fn NVSDK_NGX_Parameter_GetI(params: *mut c_void, name: *const c_char, value: *mut i32) -> u32;
    fn NVSDK_NGX_Parameter_SetUI(params: *mut c_void, name: *const c_char, value: u32);
    fn NVSDK_NGX_Parameter_SetD3d11Resource(
        params: *mut c_void,
        name: *const c_char,
        value: *mut c_void,
    );
}
struct Runtime {
    device: ID3D11Device,
    users: usize,
}
static DEVICES: Mutex<BTreeMap<usize, Runtime>> = Mutex::new(BTreeMap::new());
struct TrueHdr {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    params: *mut c_void,
    feature: *mut c_void,
    output: Option<ID3D11Texture2D>,
    size: (u32, u32),
    acquired: bool,
}
fn check(code: u32) -> Result<(), u32> {
    if code == 1 { Ok(()) } else { Err(code) }
}
impl TrueHdr {
    unsafe fn new(device: ID3D11Device) -> Result<Box<Self>, u32> {
        unsafe {
            // Do not enter NGX on an unsupported adapter.
            let adapter = device
                .cast::<IDXGIDevice>()
                .map_err(|e| e.code().0 as u32)?
                .GetAdapter()
                .map_err(|e| e.code().0 as u32)?
                .GetDesc()
                .map_err(|e| e.code().0 as u32)?;
            if adapter.VendorId != 0x10de {
                return Err(0xbad00009);
            }
            let context = device
                .GetImmediateContext()
                .map_err(|e| e.code().0 as u32)?;
            let mut state = Box::new(Self {
                device,
                context,
                params: ptr::null_mut(),
                feature: ptr::null_mut(),
                output: None,
                size: (0, 0),
                acquired: false,
            });
            let result = (|| -> Result<(), u32> {
                let mut devices = DEVICES.lock().unwrap();
                let id = state.device.as_raw() as usize;
                if let Some(runtime) = devices.get_mut(&id) {
                    runtime.users += 1;
                } else {
                    let path = [b'.' as u16, 0];
                    check(NVSDK_NGX_D3D11_Init(
                        0,
                        path.as_ptr(),
                        state.device.as_raw(),
                        0x15,
                    ))?;
                    devices.insert(
                        id,
                        Runtime {
                            device: state.device.clone(),
                            users: 1,
                        },
                    );
                }
                state.acquired = true;
                check(NVSDK_NGX_D3D11_GetCapabilityParameters(&mut state.params))?;
                if state.params.is_null() {
                    return Err(0xbad00000);
                }
                let mut available = 0;
                check(NVSDK_NGX_Parameter_GetI(
                    state.params,
                    c"TrueHDR.Available".as_ptr(),
                    &mut available,
                ))?;
                if available == 0 {
                    return Err(0xbad00009);
                }
                check(NVSDK_NGX_D3D11_CreateFeature(
                    state.context.as_raw(),
                    14,
                    state.params,
                    &mut state.feature,
                ))?;
                Ok(())
            })();
            result?;
            Ok(state)
        }
    }
    unsafe fn convert(
        &mut self,
        input: &ID3D11Texture2D,
        contrast: u32,
        saturation: u32,
        middle: u32,
        peak: u32,
    ) -> Result<*mut c_void, u32> {
        unsafe {
            let _devices = DEVICES.lock().unwrap();
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            input.GetDesc(&mut desc);
            if !matches!(
                desc.Format,
                DXGI_FORMAT_B8G8R8A8_UNORM
                    | DXGI_FORMAT_R8G8B8A8_UNORM
                    | DXGI_FORMAT_R10G10B10A2_UNORM
            ) {
                return Err(0xbad00005);
            }
            if desc.Width == 0 || desc.Height == 0 || desc.Width > 16384 || desc.Height > 16384 {
                return Err(0xbad00005);
            }
            if self.size != (desc.Width, desc.Height) {
                self.output = None;
                let output = D3D11_TEXTURE2D_DESC {
                    Width: desc.Width,
                    Height: desc.Height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_SHADER_RESOURCE.0
                        | D3D11_BIND_RENDER_TARGET.0
                        | D3D11_BIND_UNORDERED_ACCESS.0) as u32,
                    ..Default::default()
                };
                self.device
                    .CreateTexture2D(&output, None, Some(&mut self.output))
                    .map_err(|e| e.code().0 as u32)?;
                self.size = (desc.Width, desc.Height);
            }
            let out = self.output.as_ref().unwrap();
            NVSDK_NGX_Parameter_SetD3d11Resource(self.params, c"Input1".as_ptr(), input.as_raw());
            NVSDK_NGX_Parameter_SetD3d11Resource(self.params, c"Output".as_ptr(), out.as_raw());
            for (name, value) in [
                (c"TrueHDR.InLeft", 0),
                (c"TrueHDR.InTop", 0),
                (c"TrueHDR.InRight", desc.Width),
                (c"TrueHDR.InBottom", desc.Height),
                (c"TrueHDR.OutLeft", 0),
                (c"TrueHDR.OutTop", 0),
                (c"TrueHDR.OutRight", desc.Width),
                (c"TrueHDR.OutBottom", desc.Height),
                (c"TrueHDR.Contrast", contrast.min(200)),
                (c"TrueHDR.Saturation", saturation.min(200)),
                (c"TrueHDR.MiddleGray", middle.clamp(10, 100)),
                (c"TrueHDR.MaxLuminance", peak.clamp(400, 2000)),
            ] {
                NVSDK_NGX_Parameter_SetUI(self.params, name.as_ptr(), value);
            }
            check(NVSDK_NGX_D3D11_EvaluateFeature_C(
                self.context.as_raw(),
                self.feature,
                self.params,
                ptr::null(),
            ))?;
            Ok(out.as_raw())
        }
    }
}
impl Drop for TrueHdr {
    fn drop(&mut self) {
        unsafe {
            self.output = None;
            let releases = {
                let mut devices = DEVICES.lock().unwrap();
                if !self.feature.is_null() {
                    NVSDK_NGX_D3D11_ReleaseFeature(self.feature);
                }
                if !self.params.is_null() {
                    NVSDK_NGX_D3D11_DestroyParameters(self.params);
                }
                if self.acquired
                    && let Some(runtime) = devices.get_mut(&(self.device.as_raw() as usize))
                {
                    runtime.users -= 1;
                }
                if devices.values().all(|d| d.users == 0) {
                    for runtime in devices.values() {
                        NVSDK_NGX_D3D11_Shutdown1(runtime.device.as_raw());
                    }
                    std::mem::take(&mut *devices)
                } else {
                    BTreeMap::new()
                }
            };
            // A final D3D device release can wait for display removal. Never hold the NGX lock here.
            drop(releases);
        }
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn butterpollo_truehdr_abi() -> u32 {
    1
}
/// Create a converter borrowing a D3D11 device for the duration of the call.
/// # Safety
/// `device` must be null or a valid ID3D11Device interface; a non-null `error`
/// must point to a writable u32. Destroy the returned state exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn butterpollo_truehdr_create(
    device: *mut c_void,
    error: *mut u32,
) -> *mut c_void {
    unsafe {
        if !error.is_null() {
            *error = 0;
        }
        let Some(device) = ID3D11Device::from_raw_borrowed(&device) else {
            return ptr::null_mut();
        };
        match TrueHdr::new(device.clone()) {
            Ok(state) => Box::into_raw(state).cast(),
            Err(code) => {
                if !error.is_null() {
                    *error = code;
                }
                ptr::null_mut()
            }
        }
    }
}
/// Convert an SDR texture; the returned texture remains owned by the state.
/// # Safety
/// `state` must be null or a live state returned by create; `input` must be
/// null or a valid ID3D11Texture2D on its device. A non-null `error` must be
/// writable. Calls on one state must be serialized and use its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn butterpollo_truehdr_convert(
    state: *mut c_void,
    input: *mut c_void,
    contrast: u32,
    saturation: u32,
    middle: u32,
    peak: u32,
    error: *mut u32,
) -> *mut c_void {
    unsafe {
        if !error.is_null() {
            *error = 0;
        }
        let Some(state) = state.cast::<TrueHdr>().as_mut() else {
            return ptr::null_mut();
        };
        let Some(input) = ID3D11Texture2D::from_raw_borrowed(&input) else {
            return ptr::null_mut();
        };
        match state.convert(input, contrast, saturation, middle, peak) {
            Ok(out) => out,
            Err(code) => {
                if !error.is_null() {
                    *error = code;
                }
                ptr::null_mut()
            }
        }
    }
}
/// Release a converter and its feature resources.
/// # Safety
/// `state` must be null or a state returned by create, with no concurrent calls;
/// a non-null state must be destroyed exactly once on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn butterpollo_truehdr_destroy(state: *mut c_void) {
    if !state.is_null() {
        unsafe {
            drop(Box::from_raw(state.cast::<TrueHdr>()));
        }
    }
}
