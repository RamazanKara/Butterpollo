//! Vulkan implicit layer that offers HDR surface formats on a virtual display.
//!
//! While a Rubylight stream holds its HDR lease (a named event), the layer
//! adds HDR10 (A2B10G10R10, ST2084) and scRGB (R16G16B16A16 float, extended
//! linear) to the formats a Win32 surface reports, so games can enable HDR on
//! a virtual display whose driver does not advertise it. Every other call is
//! passed to the next layer unchanged. The ABI follows Vulkan's `vk_layer.h`
//! and the libvirtualdisplay layer this one replaces.
//!
//! All entry points are called by the Vulkan loader, which guarantees the
//! pointer arguments Vulkan's valid-usage rules describe; each `unsafe` block
//! relies on that contract.
#![warn(clippy::undocumented_unsafe_blocks, clippy::missing_safety_doc)]
use std::{
    collections::BTreeMap,
    ffi::{CStr, c_char, c_void},
    ptr,
    sync::{Mutex, OnceLock},
};
type Handle = *mut c_void;
type Function = Option<unsafe extern "system" fn()>;
type Gipa = unsafe extern "system" fn(Handle, *const c_char) -> Function;
type Create = unsafe extern "system" fn(*const CreateInfo, *const c_void, *mut Handle) -> i32;
type Destroy = unsafe extern "system" fn(Handle, *const c_void);
type CreateSurface =
    unsafe extern "system" fn(Handle, *const Win32Surface, *const c_void, *mut u64) -> i32;
type DestroySurface = unsafe extern "system" fn(Handle, u64, *const c_void);
type Formats = unsafe extern "system" fn(Handle, u64, *mut u32, *mut Format) -> i32;
type Formats2 =
    unsafe extern "system" fn(Handle, *const SurfaceInfo, *mut u32, *mut Format2) -> i32;
const SUCCESS: i32 = 0;
const INCOMPLETE: i32 = 5;
const FAILED: i32 = -3;
const MAX_FORMATS: u32 = 4096;
#[repr(C)]
pub struct CreateInfo {
    kind: u32,
    next: *const c_void,
    flags: u32,
    app: *const c_void,
    layers: u32,
    layer_names: *const *const c_char,
    extensions: u32,
    extension_names: *const *const c_char,
}
#[repr(C)]
pub struct Win32Surface {
    kind: u32,
    next: *const c_void,
    flags: u32,
    instance: Handle,
    window: Handle,
}
#[repr(C)]
pub struct SurfaceInfo {
    kind: u32,
    next: *const c_void,
    surface: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Format {
    format: i32,
    color_space: i32,
}
#[repr(C)]
pub struct Format2 {
    kind: u32,
    next: *mut c_void,
    format: Format,
}
#[repr(C)]
struct Link {
    next: *mut Link,
    gipa: Gipa,
    physical_gipa: Function,
}
#[repr(C)]
struct Chain {
    kind: u32,
    next: *const c_void,
    function: i32,
    link: *mut Link,
}
#[repr(C)]
struct DeviceLink {
    next: *mut DeviceLink,
    gipa: Gipa,
    gdpa: Gipa,
}
#[repr(C)]
struct DeviceChain {
    kind: u32,
    next: *const c_void,
    function: i32,
    link: *mut DeviceLink,
}
#[repr(C)]
pub struct Negotiation {
    kind: i32,
    next: *mut c_void,
    version: u32,
    gipa: Option<Gipa>,
    gdpa: Option<Gipa>,
    physical_gipa: Option<Gipa>,
}
#[derive(Clone, Copy)]
struct Instance {
    handle: usize,
    next: Gipa,
    destroy: Option<Destroy>,
    create_surface: Option<CreateSurface>,
    destroy_surface: Option<DestroySurface>,
    formats: Option<Formats>,
    formats2: Option<Formats2>,
}
#[derive(Default)]
struct State {
    instances: BTreeMap<usize, Instance>,
    devices: BTreeMap<usize, Gipa>,
    surfaces: BTreeMap<u64, (usize, usize)>,
}
fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Mutex::default)
}
/// The loader's dispatch table pointer, which identifies the instance or
/// device behind a dispatchable handle and is shared by its child objects.
///
/// # Safety
/// `handle` is null or a dispatchable Vulkan handle.
unsafe fn key(handle: Handle) -> Option<usize> {
    // SAFETY: a dispatchable handle points at its dispatch table pointer.
    unsafe {
        if handle.is_null() {
            None
        } else {
            Some(*(handle.cast::<usize>()))
        }
    }
}
/// The instance a dispatchable handle belongs to.
///
/// # Safety
/// `handle` is null or a dispatchable Vulkan handle, as for [`key`].
unsafe fn instance(handle: Handle) -> Option<Instance> {
    // SAFETY: forwarded from the caller.
    let key = unsafe { key(handle) }?;
    state().lock().ok()?.instances.get(&key).copied()
}
fn extend(formats: &mut Vec<Format>) {
    for additional in [
        Format {
            format: 97,
            color_space: 1000104002,
        },
        Format {
            format: 64,
            color_space: 1000104008,
        },
    ] {
        if !formats
            .iter()
            .any(|f| f.color_space == additional.color_space)
        {
            formats.push(additional);
        }
    }
}
#[cfg(windows)]
fn stream_active() -> bool {
    use windows::{
        Win32::{
            Foundation::{CloseHandle, WAIT_OBJECT_0},
            System::Threading::*,
        },
        core::PCWSTR,
    };
    for name in [
        "Global\\ButterpolloRustVirtualHdrActive",
        "Local\\ButterpolloRustVirtualHdrActive",
    ] {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call.
        let opened =
            unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(name.as_ptr())) };
        if let Ok(event) = opened {
            // SAFETY: `event` is the handle just opened.
            let active = unsafe { WaitForSingleObject(event, 0) == WAIT_OBJECT_0 };
            // SAFETY: `event` is open and not used afterwards.
            unsafe {
                let _ = CloseHandle(event);
            }
            if active {
                return true;
            }
        }
    }
    false
}
#[cfg(not(windows))]
fn stream_active() -> bool {
    false
}
#[cfg(windows)]
fn hdr_supported(window: usize) -> bool {
    use windows::Win32::{Devices::Display::*, Foundation::*, Graphics::Gdi::*};
    // SAFETY: every pointer passed is to a local of the size its header
    // states, and the path and mode buffers hold the counts Windows reported.
    unsafe {
        let monitor = MonitorFromWindow(HWND(window as Handle), MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if monitor.is_invalid()
            || !GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast()).as_bool()
        {
            return false;
        }
        let (mut paths_len, mut modes_len) = (0, 0);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths_len, &mut modes_len)
            .is_err()
            || paths_len > 256
            || modes_len > 768
        {
            return false;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); paths_len as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); modes_len as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut paths_len,
            paths.as_mut_ptr(),
            &mut modes_len,
            modes.as_mut_ptr(),
            None,
        )
        .is_err()
        {
            return false;
        }
        for path in &paths[..paths_len as usize] {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0
                || source.viewGdiDeviceName != info.szDevice
            {
                continue;
            }
            // Windows 11 24H2 distinguishes HDR from automatic wide color.
            #[repr(C)]
            struct Advanced2 {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
                flags: u32,
                encoding: u32,
                bits: u32,
                active: u32,
            }
            let mut color = Advanced2 {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_TYPE(15),
                    size: size_of::<Advanced2>() as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                flags: 0,
                encoding: 0,
                bits: 0,
                active: 0,
            };
            if DisplayConfigGetDeviceInfo(&mut color.header) == 0 {
                return color.flags & 16 != 0;
            }
            let mut old = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                    size: size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                ..Default::default()
            };
            return DisplayConfigGetDeviceInfo(&mut old.header) == 0
                && old.Anonymous.value & 1 != 0;
        }
        false
    }
}
#[cfg(not(windows))]
fn hdr_supported(_: usize) -> bool {
    false
}
fn inject(surface: u64) -> bool {
    if !stream_active() || std::env::var("DISABLE_SUNSHINE_VIRTUAL_HDR").is_ok_and(|v| v == "1") {
        return false;
    }
    let window = state()
        .lock()
        .ok()
        .and_then(|s| s.surfaces.get(&surface).map(|(_, window)| *window));
    window.is_some_and(|window| {
        std::env::var("SUNSHINE_VHDR_FORCE").is_ok_and(|v| v.starts_with('1'))
            || hdr_supported(window)
    })
}
unsafe extern "system" fn create(
    info: *const CreateInfo,
    allocator: *const c_void,
    result: *mut Handle,
) -> i32 {
    // SAFETY: called as vkCreateInstance with a valid create info whose pNext chain
    // holds the loader's layer link (VK_STRUCTURE_TYPE_LOADER_INSTANCE_CREATE_INFO,
    // 47); the next layer's vkGetInstanceProcAddr returns functions with the
    // signatures they are transmuted to.
    unsafe {
        if info.is_null() || result.is_null() {
            return FAILED;
        }
        let mut chain = (*info).next as *mut Chain;
        for _ in 0..128 {
            if chain.is_null() {
                return FAILED;
            }
            if (*chain).kind == 47 && (*chain).function == 0 {
                break;
            }
            chain = (*chain).next as *mut Chain;
        }
        if chain.is_null()
            || (*chain).kind != 47
            || (*chain).function != 0
            || (*chain).link.is_null()
        {
            return FAILED;
        }
        let link = (*chain).link;
        let next = (*link).gipa;
        (*chain).link = (*link).next;
        let Some(next_create) = next(ptr::null_mut(), c"vkCreateInstance".as_ptr()) else {
            return FAILED;
        };
        let next_create: Create = std::mem::transmute(next_create);
        let code = next_create(info, allocator, result);
        if code != SUCCESS {
            return code;
        }
        let Some(key) = key(*result) else {
            return FAILED;
        };
        let data = Instance {
            handle: *result as usize,
            next,
            destroy: std::mem::transmute::<Function, Option<Destroy>>(next(
                *result,
                c"vkDestroyInstance".as_ptr(),
            )),
            create_surface: std::mem::transmute::<Function, Option<CreateSurface>>(next(
                *result,
                c"vkCreateWin32SurfaceKHR".as_ptr(),
            )),
            destroy_surface: std::mem::transmute::<Function, Option<DestroySurface>>(next(
                *result,
                c"vkDestroySurfaceKHR".as_ptr(),
            )),
            formats: std::mem::transmute::<Function, Option<Formats>>(next(
                *result,
                c"vkGetPhysicalDeviceSurfaceFormatsKHR".as_ptr(),
            )),
            formats2: std::mem::transmute::<Function, Option<Formats2>>(next(
                *result,
                c"vkGetPhysicalDeviceSurfaceFormats2KHR".as_ptr(),
            )),
        };
        if let Ok(mut s) = state().lock() {
            s.instances.insert(key, data);
        }
        SUCCESS
    }
}
unsafe extern "system" fn destroy(handle: Handle, allocator: *const c_void) {
    // SAFETY: called as vkDestroyInstance on an instance created through `create`;
    // `next` is that instance's own vkDestroyInstance.
    unsafe {
        let data = key(handle).and_then(|key| {
            state().lock().ok().and_then(|mut s| {
                s.surfaces.retain(|_, (owner, _)| *owner != key);
                s.instances.remove(&key)
            })
        });
        if let Some(next) = data.and_then(|d| d.destroy) {
            next(handle, allocator);
        }
    }
}
unsafe extern "system" fn create_surface(
    handle: Handle,
    info: *const Win32Surface,
    allocator: *const c_void,
    surface: *mut u64,
) -> i32 {
    // SAFETY: called as vkCreateWin32SurfaceKHR; `info` and `surface` are valid
    // when non-null, and `surface` holds the new handle on success.
    unsafe {
        let Some(next) = instance(handle).and_then(|d| d.create_surface) else {
            return FAILED;
        };
        let code = next(handle, info, allocator, surface);
        if code == SUCCESS
            && !info.is_null()
            && !surface.is_null()
            && let Some(owner) = key(handle)
            && let Ok(mut s) = state().lock()
        {
            s.surfaces
                .insert(*surface, (owner, (*info).window as usize));
        }
        code
    }
}
unsafe extern "system" fn destroy_surface(handle: Handle, surface: u64, allocator: *const c_void) {
    // SAFETY: called as vkDestroySurfaceKHR on an instance this layer recorded.
    unsafe {
        if let Ok(mut s) = state().lock() {
            s.surfaces.remove(&surface);
        }
        if let Some(next) = instance(handle).and_then(|d| d.destroy_surface) {
            next(handle, surface, allocator);
        }
    }
}
unsafe extern "system" fn create_device(
    physical: Handle,
    info: *const c_void,
    allocator: *const c_void,
    device: *mut Handle,
) -> i32 {
    // SAFETY: called as vkCreateDevice with a valid create info whose pNext chain
    // holds the loader's layer link (VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO,
    // 48); the next layer's functions have the signatures they are transmuted to.
    unsafe {
        if info.is_null() || device.is_null() {
            return FAILED;
        }
        let mut chain = (*(info.cast::<CreateInfo>())).next as *mut DeviceChain;
        for _ in 0..128 {
            if chain.is_null() {
                return FAILED;
            }
            if (*chain).kind == 48 && (*chain).function == 0 {
                break;
            }
            chain = (*chain).next as *mut DeviceChain;
        }
        if chain.is_null()
            || (*chain).kind != 48
            || (*chain).function != 0
            || (*chain).link.is_null()
        {
            return FAILED;
        }
        let link = (*chain).link;
        let gipa = (*link).gipa;
        let gdpa = (*link).gdpa;
        (*chain).link = (*link).next;
        let Some(data) = instance(physical) else {
            return FAILED;
        };
        let Some(next) = gipa(data.handle as Handle, c"vkCreateDevice".as_ptr()) else {
            return FAILED;
        };
        let next: unsafe extern "system" fn(
            Handle,
            *const c_void,
            *const c_void,
            *mut Handle,
        ) -> i32 = std::mem::transmute(next);
        let result = next(physical, info, allocator, device);
        if result == SUCCESS
            && let Some(key) = key(*device)
            && let Ok(mut state) = state().lock()
        {
            state.devices.insert(key, gdpa);
        }
        result
    }
}
unsafe extern "system" fn destroy_device(device: Handle, allocator: *const c_void) {
    // SAFETY: called as vkDestroyDevice on a device created through `create_device`,
    // whose recorded vkGetDeviceProcAddr returns its vkDestroyDevice.
    unsafe {
        let next = key(device)
            .and_then(|key| state().lock().ok().and_then(|mut s| s.devices.remove(&key)));
        if let Some(next) = next.and_then(|gdpa| gdpa(device, c"vkDestroyDevice".as_ptr())) {
            let next: Destroy = std::mem::transmute(next);
            next(device, allocator);
        }
    }
}
unsafe fn copy_formats(
    formats: &[Format],
    count: *mut u32,
    mut write: impl FnMut(usize, Format),
) -> i32 {
    // SAFETY: `count` is valid, and `write` stores only below the count the caller
    // provided, which is the room its buffer has.
    unsafe {
        let written = (*count as usize).min(formats.len());
        for (i, format) in formats[..written].iter().enumerate() {
            write(i, *format);
        }
        *count = written as u32;
        if written < formats.len() {
            INCOMPLETE
        } else {
            SUCCESS
        }
    }
}
unsafe extern "system" fn formats(
    device: Handle,
    surface: u64,
    count: *mut u32,
    out: *mut Format,
) -> i32 {
    // SAFETY: called as vkGetPhysicalDeviceSurfaceFormatsKHR: `count` is valid when
    // non-null and `out`, when non-null, has room for `*count` formats.
    unsafe {
        let Some(next) = instance(device).and_then(|d| d.formats) else {
            return FAILED;
        };
        if count.is_null() {
            return FAILED;
        }
        if !inject(surface) {
            return next(device, surface, count, out);
        }
        let mut base = 0;
        if next(device, surface, &mut base, ptr::null_mut()) != SUCCESS || base > MAX_FORMATS {
            return next(device, surface, count, out);
        }
        let mut values = vec![Format::default(); base as usize];
        let code = next(device, surface, &mut base, values.as_mut_ptr());
        if !matches!(code, SUCCESS | INCOMPLETE) || base as usize > values.len() {
            return next(device, surface, count, out);
        }
        values.truncate(base as usize);
        extend(&mut values);
        if out.is_null() {
            *count = values.len() as u32;
            SUCCESS
        } else {
            copy_formats(&values, count, |i, format| *out.add(i) = format)
        }
    }
}
unsafe extern "system" fn formats2(
    device: Handle,
    info: *const SurfaceInfo,
    count: *mut u32,
    out: *mut Format2,
) -> i32 {
    // SAFETY: called as vkGetPhysicalDeviceSurfaceFormats2KHR: `info` and `count`
    // are valid when non-null and `out`, when non-null, has room for `*count`
    // entries whose sType and pNext the caller set.
    unsafe {
        let Some(next) = instance(device).and_then(|d| d.formats2) else {
            return FAILED;
        };
        if count.is_null() {
            return FAILED;
        }
        if info.is_null() || !inject((*info).surface) {
            return next(device, info, count, out);
        }
        let mut base = 0;
        if next(device, info, &mut base, ptr::null_mut()) != SUCCESS || base > MAX_FORMATS {
            return next(device, info, count, out);
        }
        let mut values: Vec<_> = (0..base)
            .map(|_| Format2 {
                kind: 1000119002,
                next: ptr::null_mut(),
                format: Format::default(),
            })
            .collect();
        let code = next(device, info, &mut base, values.as_mut_ptr());
        if !matches!(code, SUCCESS | INCOMPLETE) || base as usize > values.len() {
            return next(device, info, count, out);
        }
        let mut values: Vec<_> = values[..base as usize].iter().map(|v| v.format).collect();
        extend(&mut values);
        if out.is_null() {
            *count = values.len() as u32;
            SUCCESS
        }
        // Preserve each caller-owned sType and pNext chain.
        else {
            copy_formats(&values, count, |i, format| (*out.add(i)).format = format)
        }
    }
}
/// The layer's `vkGetInstanceProcAddr`: its own entry points by name,
/// everything else from the next layer.
///
/// # Safety
/// Called by the Vulkan loader: `handle` is null or a `VkInstance`, and
/// `name` is null or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetInstanceProcAddr(
    handle: Handle,
    name: *const c_char,
) -> Function {
    // SAFETY: `name` is NUL-terminated; each returned entry point is erased to
    // PFN_vkVoidFunction, as the API defines.
    unsafe {
        if name.is_null() {
            return None;
        }
        let f: *const () = match CStr::from_ptr(name).to_bytes() {
            b"vkGetInstanceProcAddr" => vkGetInstanceProcAddr as *const (),
            b"vkCreateInstance" => create as *const (),
            b"vkCreateDevice" => create_device as *const (),
            b"vkGetDeviceProcAddr" => vkGetDeviceProcAddr as *const (),
            b"vkDestroyInstance" => destroy as *const (),
            b"vkCreateWin32SurfaceKHR" => create_surface as *const (),
            b"vkDestroySurfaceKHR" => destroy_surface as *const (),
            b"vkGetPhysicalDeviceSurfaceFormatsKHR" => formats as *const (),
            b"vkGetPhysicalDeviceSurfaceFormats2KHR" => formats2 as *const (),
            _ => return instance(handle).and_then(|d| (d.next)(handle, name)),
        };
        Some(std::mem::transmute::<*const (), unsafe extern "system" fn()>(f))
    }
}
/// The layer's `vkGetDeviceProcAddr`.
///
/// # Safety
/// Called by the Vulkan loader: `device` is a `VkDevice` created through
/// this layer, and `name` is null or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetDeviceProcAddr(device: Handle, name: *const c_char) -> Function {
    // SAFETY: `name` is NUL-terminated; each returned entry point is erased to
    // PFN_vkVoidFunction, as the API defines.
    unsafe {
        if name.is_null() {
            return None;
        }
        match CStr::from_ptr(name).to_bytes() {
            b"vkGetDeviceProcAddr" => {
                return Some(
                    std::mem::transmute::<*const (), unsafe extern "system" fn()>(
                        vkGetDeviceProcAddr as *const (),
                    ),
                );
            }
            b"vkDestroyDevice" => {
                return Some(
                    std::mem::transmute::<*const (), unsafe extern "system" fn()>(
                        destroy_device as *const (),
                    ),
                );
            }
            _ => {}
        }
        let gdpa = key(device).and_then(|key| {
            state()
                .lock()
                .ok()
                .and_then(|s| s.devices.get(&key).copied())
        });
        gdpa.and_then(|gdpa| gdpa(device, name))
    }
}
/// Loader-layer interface negotiation (version 2).
///
/// # Safety
/// `info` is null or a valid `VkNegotiateLayerInterface`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(
    info: *mut Negotiation,
) -> i32 {
    // SAFETY: the loader passes a valid VkNegotiateLayerInterface or null.
    unsafe {
        if info.is_null() || (*info).kind != 1 || (*info).version < 1 {
            return FAILED;
        }
        (*info).version = (*info).version.min(2);
        (*info).gipa = Some(vkGetInstanceProcAddr);
        (*info).gdpa = Some(vkGetDeviceProcAddr);
        (*info).physical_gipa = Some(vkGetInstanceProcAddr);
        SUCCESS
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats_are_not_duplicated_and_short_buffers_are_bounded() {
        let sdr = Format {
            format: 44,
            color_space: 0,
        };
        let mut values = vec![sdr];
        extend(&mut values);
        extend(&mut values);
        assert_eq!(values.len(), 3);
        let mut buffer = [Format::default(); 2];
        let mut count = 1;
        assert_eq!(
            // SAFETY: `count` is a local and the closure indexes a two-entry buffer.
            unsafe { copy_formats(&values, &mut count, |i, format| buffer[i] = format) },
            INCOMPLETE
        );
        assert_eq!(count, 1);
        assert_eq!(buffer[0], sdr);
        assert_eq!(buffer[1], Format::default());
    }
}
