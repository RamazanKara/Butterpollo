//! Per-user Vulkan layer registration and reference-counted HDR activation.

use crate::text::to_wide;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use windows::{
    Win32::{
        Foundation::*,
        Security::{
            Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
        System::{Registry::*, Threading::*},
    },
    core::PCWSTR,
};
const KEY: &str = "Software\\Khronos\\Vulkan\\ImplicitLayers";
pub fn manifest() -> Result<PathBuf> {
    let path = std::env::current_exe()?
        .parent()
        .context("executable directory missing")?
        .join("vulkan-layer/VkLayer_butterpollo_hdr.json");
    Ok(path)
}
pub fn installed(path: &Path) -> bool {
    let name = to_wide(&path.to_string_lossy());
    let key = to_wide(KEY);
    let mut data = 0u32;
    // SAFETY: `key` and `name` are NUL-terminated and outlive the calls, and `data` is the 4-byte
    // buffer `size` names. `size` is reset per root because a failed query overwrites it.
    unsafe {
        [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
            .into_iter()
            .any(|root| {
                let mut size = 4u32;
                RegGetValueW(
                    root,
                    PCWSTR(key.as_ptr()),
                    PCWSTR(name.as_ptr()),
                    RRF_RT_REG_DWORD,
                    None,
                    Some((&mut data as *mut u32).cast()),
                    Some(&mut size),
                )
                .is_ok()
                    && data == 0
            })
    }
}
pub fn register(enabled: bool) -> Result<()> {
    let path = manifest()?;
    if enabled
        && (!path.is_file()
            || !path
                .with_file_name("butterpollo_vulkan_layer.dll")
                .is_file())
    {
        bail!("the Rust Vulkan HDR layer is missing from this installation");
    }
    let key = to_wide(KEY);
    let name = to_wide(&path.to_string_lossy());
    // SAFETY: `key`, `name` and `value` outlive the calls, and `handle` is closed once, only after
    // it was opened successfully.
    unsafe {
        let mut handle = HKEY::default();
        RegCreateKeyExW(
            if crate::process::is_system() {
                HKEY_LOCAL_MACHINE
            } else {
                HKEY_CURRENT_USER
            },
            PCWSTR(key.as_ptr()),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut handle,
            None,
        )
        .ok()?;
        let value = u32::from(!enabled).to_le_bytes();
        let result = RegSetValueExW(handle, PCWSTR(name.as_ptr()), None, REG_DWORD, Some(&value));
        let _ = RegCloseKey(handle);
        result.ok()?;
    }
    Ok(())
}
pub fn reconcile(enabled: bool) -> Result<()> {
    let path = manifest()?;
    if enabled || installed(&path) {
        register(enabled)?;
    }
    Ok(())
}
pub fn status(enabled: bool) -> Value {
    let path = manifest().unwrap_or_default();
    json!({"status":true,"installed":installed(&path),"enabled":enabled,
        "available":path.is_file() && path.with_file_name("butterpollo_vulkan_layer.dll").is_file(),
        "manifest":path,"active":EVENTS.get().is_some_and(|e| e.lock().is_ok_and(|e| e.users > 0))})
}
struct Events {
    users: usize,
    handles: Vec<HANDLE>,
}
// Named kernel events are thread safe; ownership and refcounts use the mutex.
// SAFETY: Events owns its handles, closes them only in Drop, and is reached only through the EVENTS
// mutex; kernel event handles may be used from any thread.
unsafe impl Send for Events {}
impl Drop for Events {
    fn drop(&mut self) {
        for handle in &self.handles {
            // SAFETY: Each handle came from CreateEventW, is owned by Events, and is closed once,
            // here.
            unsafe {
                let _ = CloseHandle(*handle);
            }
        }
    }
}
static EVENTS: OnceLock<Mutex<Events>> = OnceLock::new();
pub struct Lease;
impl Lease {
    pub fn acquire() -> Result<Self> {
        let events = EVENTS.get_or_init(|| {
            Mutex::new(Events {
                users: 0,
                handles: vec![],
            })
        });
        let mut events = events.lock().unwrap();
        if events.handles.is_empty() {
            let descriptor_text =
                to_wide("D:P(A;;GA;;;OW)(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x00100000;;;WD)");
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            // SAFETY: `descriptor_text` is NUL-terminated and outlives the call; `descriptor`
            // receives a LocalAlloc'd descriptor that is freed below.
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    PCWSTR(descriptor_text.as_ptr()),
                    1,
                    &mut descriptor,
                    None,
                )?;
            }
            let security = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            };
            for name in [
                "Global\\ButterpolloRustVirtualHdrActive",
                "Local\\ButterpolloRustVirtualHdrActive",
            ] {
                let name = to_wide(name);
                // SAFETY: `security` points at the live descriptor and `name` is NUL-terminated;
                // both outlive the call.
                let created =
                    unsafe { CreateEventW(Some(&security), true, false, PCWSTR(name.as_ptr())) };
                if let Ok(handle) = created {
                    events.handles.push(handle);
                }
            }
            // SAFETY: `descriptor` was allocated by the conversion above and is no longer used.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(descriptor.0)));
            }
            if events.handles.is_empty() {
                bail!("cannot create the Vulkan HDR activation event");
            }
        }
        if events.users == 0 {
            for handle in &events.handles {
                // SAFETY: The handle is owned by Events, which the held lock keeps alive and open.
                unsafe {
                    SetEvent(*handle)?;
                }
            }
        }
        events.users += 1;
        Ok(Self)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(events) = EVENTS.get() {
            let mut events = events.lock().unwrap();
            events.users -= 1;
            if events.users == 0 {
                for handle in &events.handles {
                    // SAFETY: The handle is owned by Events, which the held lock keeps alive and
                    // open.
                    unsafe {
                        let _ = ResetEvent(*handle);
                    }
                }
            }
        }
    }
}
