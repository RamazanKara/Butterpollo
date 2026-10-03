//! WDDM scheduling applies to this process only; no system HAGS setting changes.
use anyhow::{Context, Result, bail};
use libloading::Library;
use windows::Win32::{Foundation::LUID, System::Threading::GetCurrentProcess};
use windows::{
    Win32::Graphics::Dxgi::{IDXGIDevice, IDXGIDevice1},
    core::Interface,
};
#[repr(C)]
struct Open {
    luid: LUID,
    adapter: u32,
}
#[repr(C)]
struct Query {
    adapter: u32,
    kind: u32,
    data: *mut std::ffi::c_void,
    size: u32,
}
#[repr(C)]
struct Close {
    adapter: u32,
}
fn priority(vendor: u32, hags: bool, realtime_hags: bool) -> i32 {
    if vendor == 0x10de && hags && !realtime_hags {
        4
    } else {
        5
    }
}
pub fn configure(
    gpu: &crate::capture::Device,
    config: &butterpollo_core::config::Config,
) -> Result<()> {
    unsafe {
        let device = gpu.device.cast::<IDXGIDevice>()?;
        // These device settings are independent of the privileged process
        // scheduling class. Preserve them even if that later request is denied.
        if let Err(error) = device.SetGPUThreadPriority(7) {
            tracing::debug!(%error, "capture GPU thread priority remains at its default");
        }
        if let Err(error) = gpu.device.cast::<IDXGIDevice1>()?.SetMaximumFrameLatency(1) {
            tracing::debug!(%error, "capture GPU frame latency remains at its default");
        }
        let desc = device.GetAdapter()?.GetDesc()?;
        let directory = std::env::var_os("SystemRoot").context("Windows directory missing")?;
        let library = Library::new(std::path::PathBuf::from(directory).join("System32/gdi32.dll"))?;
        let open = library
            .get::<unsafe extern "system" fn(*mut Open) -> i32>(b"D3DKMTOpenAdapterFromLuid\0")?;
        let query = library
            .get::<unsafe extern "system" fn(*mut Query) -> i32>(b"D3DKMTQueryAdapterInfo\0")?;
        let close =
            library.get::<unsafe extern "system" fn(*mut Close) -> i32>(b"D3DKMTCloseAdapter\0")?;
        let set = library.get::<unsafe extern "system" fn(*mut std::ffi::c_void, i32) -> i32>(
            b"D3DKMTSetProcessSchedulingPriorityClass\0",
        )?;
        let mut adapter = Open {
            luid: desc.AdapterLuid,
            adapter: 0,
        };
        let mut caps = 0u32;
        let hags = if open(&mut adapter) >= 0 {
            let mut info = Query {
                adapter: adapter.adapter,
                kind: 70,
                data: (&mut caps as *mut u32).cast(),
                size: 4,
            };
            let result = query(&mut info);
            close(&mut Close {
                adapter: adapter.adapter,
            });
            result >= 0 && caps & 2 != 0
        } else {
            false
        };
        let priority = priority(
            desc.VendorId,
            hags,
            config.boolean("nvenc_realtime_hags", true),
        );
        let code = set(GetCurrentProcess().0, priority);
        if code < 0 {
            bail!("GPU process scheduling priority was denied ({code:#x})");
        }
        tracing::debug!(hags, priority, "GPU process scheduling configured");
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nvidia_hags_downgrade_and_kernel_abi_match_previous_host() {
        assert_eq!(priority(0x10de, true, false), 4);
        assert_eq!(priority(0x10de, true, true), 5);
        assert_eq!(priority(0x10de, false, false), 5);
        assert_eq!(priority(0x1002, true, false), 5);
        assert_eq!(size_of::<Open>(), 12);
        assert_eq!(size_of::<Query>(), 24);
        assert_eq!(std::mem::offset_of!(Query, data), 8);
    }
}
