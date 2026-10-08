//! Windows display configuration and streaming display leases.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    mem::size_of,
    time::{Duration, Instant},
};
use windows::{
    Win32::{Devices::Display::*, Foundation::*, Graphics::Gdi::*, System::IO::DeviceIoControl},
    core::{GUID, PCWSTR},
};

mod color_state;
mod hdr;
mod hotplug;
mod modes;
mod recovery;
pub mod self_test;
mod topology;
mod virtual_display;

pub use hdr::set_hdr;
pub use modes::{dpi_scale, highest_refresh, mode, set_dpi_scale, set_mode, virtual_scale};
pub use recovery::{
    Guard, Retained, Snapshot, apply_layout, baseline_nodes, leased_displays, restore_positions,
};
pub use topology::{Monitor, Topology, edid_refresh, monitors};
pub use virtual_display::{
    VirtualDisplay, VirtualOptions, configure_permanent, permanent_display_count,
    set_permanent_display_count, virtual_display_available, virtual_display_status, windows_11,
};

fn check(code: i32) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(code).into())
    }
}
fn wide(b: &[u16]) -> String {
    String::from_utf16_lossy(&b[..b.iter().position(|v| *v == 0).unwrap_or(b.len())])
}
fn header(
    kind: DISPLAYCONFIG_DEVICE_INFO_TYPE,
    size: usize,
    luid: LUID,
    id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size as u32,
        adapterId: luid,
        id,
    }
}

#[cfg(test)]
fn monitor(id: &str, adapter: u32, hdr: bool) -> Monitor {
    Monitor {
        device_id: id.into(),
        monitor_device_path: String::new(),
        display_name: r"\\.\DISPLAY1".into(),
        friendly_name: id.into(),
        hdr_supported: true,
        hdr_enabled: hdr,
        primary: false,
        adapter: LUID {
            LowPart: adapter,
            HighPart: 0,
        },
        target: 7,
        source: 0,
    }
}
