//! Read-only SDK color-state diagnostics. Does not change display settings.
#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::mem::size_of;
    use windows::{
        Win32::{Devices::Display::*, Graphics::Dxgi::*},
        core::Interface,
    };
    #[repr(C)]
    struct Info2 {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
        flags: u32,
        encoding: u32,
        bits: u32,
        mode: u32,
    }
    let mut rows = Vec::new();
    for m in butterpollo_windows::display::monitors()? {
        let mut info = Info2 {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_TYPE(15),
                size: size_of::<Info2>() as u32,
                adapterId: m.adapter,
                id: m.target,
            },
            flags: 0,
            encoding: 0,
            bits: 0,
            mode: 0,
        };
        let status = unsafe { DisplayConfigGetDeviceInfo(&mut info.header) };
        let mut legacy = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                size: size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
                adapterId: m.adapter,
                id: m.target,
            },
            ..Default::default()
        };
        let legacy_status = unsafe { DisplayConfigGetDeviceInfo(&mut legacy.header) };
        let gpu = butterpollo_windows::capture::Device::new(&m.display_name)?;
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
        let adapter = unsafe { factory.EnumAdapters1(gpu.display.adapter_index)? };
        let output = unsafe { adapter.EnumOutputs(gpu.display.output_index)? };
        let desc = unsafe { output.cast::<IDXGIOutput6>()?.GetDesc1()? };
        rows.push(serde_json::json!({"monitor":m,"modern_status":status,"modern_flags":info.flags,"active_mode":info.mode,"bits":info.bits,"legacy_status":legacy_status,"legacy_flags":unsafe {legacy.Anonymous.value},"dxgi_space":desc.ColorSpace.0,"dxgi_bits":desc.BitsPerColor,"max_luminance":desc.MaxLuminance}));
    }
    println!("{}", serde_json::to_string_pretty(&rows)?);
    Ok(())
}
