//! CUDA driver interop for NVENC's 10-bit planar 4:4:4 input.
//! Only the installed driver is loaded; no CUDA toolkit or CPU readback is used.
use crate::{capture::Device, cuda_abi::*};
use anyhow::{Context as _, Result, bail};
use std::{ffi::c_void, ptr, rc::Rc, sync::Arc};
use windows::{
    Win32::Graphics::{Direct3D11::ID3D11Texture2D, Dxgi::IDXGIDevice},
    core::Interface,
};

type Init = unsafe extern "C" fn(u32) -> CUresult;
type DeviceFn = unsafe extern "C" fn(*mut CUdevice, *mut c_void) -> CUresult;
type Create = unsafe extern "C" fn(*mut CUcontext, u32, CUdevice) -> CUresult;
type ContextFn = unsafe extern "C" fn(CUcontext) -> CUresult;
type Pop = unsafe extern "C" fn(*mut CUcontext) -> CUresult;
type Register = unsafe extern "C" fn(*mut CUgraphicsResource, *mut c_void, u32) -> CUresult;
type ResourceFn = unsafe extern "C" fn(CUgraphicsResource) -> CUresult;
type Map = unsafe extern "C" fn(u32, *mut CUgraphicsResource, CUstream) -> CUresult;
type Array = unsafe extern "C" fn(*mut CUarray, CUgraphicsResource, u32, u32) -> CUresult;
type Allocate = unsafe extern "C" fn(*mut CUdeviceptr, *mut usize, usize, usize, u32) -> CUresult;
type Free = unsafe extern "C" fn(CUdeviceptr) -> CUresult;
type Copy2d = unsafe extern "C" fn(*const CUDA_MEMCPY2D) -> CUresult;

struct Api {
    _library: libloading::Library,
    push: ContextFn,
    pop: Pop,
    destroy: ContextFn,
    register: Register,
    unregister: ResourceFn,
    map: Map,
    unmap: Map,
    array: Array,
    allocate: Allocate,
    free: Free,
    copy: Copy2d,
}
fn check(status: CUresult, operation: &str) -> Result<()> {
    if status != 0 {
        bail!("CUDA {operation} failed ({status})");
    }
    Ok(())
}
pub(crate) struct Context {
    api: Arc<Api>,
    raw: CUcontext,
}
pub(crate) struct Guard {
    context: Rc<Context>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        let mut popped = ptr::null_mut();
        if let Err(error) = check(
            unsafe { (self.context.api.pop)(&mut popped) },
            "pop context",
        ) {
            tracing::error!(%error, "CUDA context restoration failed");
        }
    }
}
impl Context {
    pub fn new(device: &Device) -> Result<Rc<Self>> {
        unsafe {
            let library = crate::nvenc::system_library("nvcuda.dll")?;
            let init: Init = *library.get(b"cuInit\0")?;
            check(init(0), "initialize driver")?;
            let get_device: DeviceFn = *library.get(b"cuD3D11GetDevice\0")?;
            let adapter = device.device.cast::<IDXGIDevice>()?.GetAdapter()?;
            let mut cu_device = 0;
            check(
                get_device(&mut cu_device, adapter.as_raw()),
                "resolve D3D11 adapter",
            )?;
            let create: Create = *library.get(b"cuCtxCreate_v2\0")?;
            let api = Arc::new(Api {
                push: *library.get(b"cuCtxPushCurrent_v2\0")?,
                pop: *library.get(b"cuCtxPopCurrent_v2\0")?,
                destroy: *library.get(b"cuCtxDestroy_v2\0")?,
                register: *library.get(b"cuGraphicsD3D11RegisterResource\0")?,
                unregister: *library.get(b"cuGraphicsUnregisterResource\0")?,
                map: *library.get(b"cuGraphicsMapResources\0")?,
                unmap: *library.get(b"cuGraphicsUnmapResources\0")?,
                array: *library.get(b"cuGraphicsSubResourceGetMappedArray\0")?,
                allocate: *library.get(b"cuMemAllocPitch_v2\0")?,
                free: *library.get(b"cuMemFree_v2\0")?,
                copy: *library.get(b"cuMemcpy2D_v2\0")?,
                _library: library,
            });
            let mut raw = ptr::null_mut();
            let status = create(&mut raw, CU_CTX_SCHED_BLOCKING_SYNC, cu_device);
            if status != 0 {
                if !raw.is_null() {
                    let _ = (api.destroy)(raw);
                }
                check(status, "create context")?;
            }
            if raw.is_null() {
                bail!("CUDA returned no context");
            }
            // cuCtxCreate pushes its context. Restore the caller before returning.
            let context = Rc::new(Self { api, raw });
            let mut popped = ptr::null_mut();
            check(
                (context.api.pop)(&mut popped),
                "restore context after creation",
            )?;
            if popped != raw {
                bail!("CUDA context stack changed during creation");
            }
            Ok(context)
        }
    }
    pub fn raw(&self) -> *mut c_void {
        self.raw.cast()
    }
    pub fn enter(self: &Rc<Self>) -> Result<Guard> {
        check(unsafe { (self.api.push)(self.raw) }, "push context")?;
        Ok(Guard {
            context: self.clone(),
        })
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        let _ = check(unsafe { (self.api.destroy)(self.raw) }, "destroy context");
    }
}

pub(crate) struct Input {
    context: Rc<Context>,
    _texture: ID3D11Texture2D,
    resource: CUgraphicsResource,
    pub pointer: CUdeviceptr,
    pub pitch: u32,
    width: u32,
    rows: u32,
    mapped: bool,
}
impl Input {
    pub fn new(
        context: &Rc<Context>,
        texture: &ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let _guard = context.enter()?;
        let mut input = Self {
            context: context.clone(),
            _texture: texture.clone(),
            resource: ptr::null_mut(),
            pointer: 0,
            pitch: 0,
            width,
            rows: height.checked_mul(3).context("4:4:4 height overflow")?,
            mapped: false,
        };
        unsafe {
            check(
                (context.api.register)(&mut input.resource, texture.as_raw(), 0),
                "register D3D11 texture",
            )?;
            if input.resource.is_null() {
                bail!("CUDA returned no registered texture");
            }
            let mut pitch = 0;
            check(
                (context.api.allocate)(
                    &mut input.pointer,
                    &mut pitch,
                    width as usize * 2,
                    input.rows as usize,
                    16,
                ),
                "allocate pitched 4:4:4 input",
            )?;
            input.pitch = u32::try_from(pitch).context("CUDA pitch exceeds the NVENC ABI")?;
            if input.pointer == 0 || pitch < width as usize * 2 {
                bail!("invalid CUDA pitched allocation");
            }
        }
        Ok(input)
    }
    pub fn copy(&mut self) -> Result<()> {
        let _guard = self.context.enter()?;
        unsafe {
            check(
                (self.context.api.map)(1, &mut self.resource, ptr::null_mut()),
                "map D3D11 input",
            )?;
            self.mapped = true;
            let result = (|| {
                let mut array = ptr::null_mut();
                check(
                    (self.context.api.array)(&mut array, self.resource, 0, 0),
                    "resolve mapped texture",
                )?;
                if array.is_null() {
                    bail!("CUDA returned no mapped array");
                }
                check(
                    (self.context.api.copy)(&CUDA_MEMCPY2D {
                        srcMemoryType: CUmemorytype_enum_CU_MEMORYTYPE_ARRAY,
                        srcArray: array,
                        dstMemoryType: CUmemorytype_enum_CU_MEMORYTYPE_DEVICE,
                        dstDevice: self.pointer,
                        dstPitch: self.pitch as usize,
                        WidthInBytes: self.width as usize * 2,
                        Height: self.rows as usize,
                        ..Default::default()
                    }),
                    "copy GPU planar input",
                )
            })();
            let unmapped = check(
                (self.context.api.unmap)(1, &mut self.resource, ptr::null_mut()),
                "unmap D3D11 input",
            );
            if unmapped.is_ok() {
                self.mapped = false;
            }
            result.and(unmapped)
        }
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        let Ok(_guard) = self.context.enter() else {
            return;
        };
        unsafe {
            if self.mapped {
                let _ = (self.context.api.unmap)(1, &mut self.resource, ptr::null_mut());
            }
            if !self.resource.is_null() {
                let _ = (self.context.api.unregister)(self.resource);
            }
            if self.pointer != 0 {
                let _ = (self.context.api.free)(self.pointer);
            }
        }
    }
}
