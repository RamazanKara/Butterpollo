//! Explicitly opted-in Windows/driver integration coverage. This creates one
//! neutral owned controller at a time; it never submits buttons, keyboard or
//! mouse input, installs a driver, or changes an existing controller.
use super::*;
use anyhow::{Context, ensure};
use std::time::{Duration, Instant};

const HID_INTERFACE: GUID = GUID::from_u128(0x4d1e55b2_f16f_11cf_88cb_001111000030);

struct DeviceSet(HDEVINFO);
impl Drop for DeviceSet {
    fn drop(&mut self) {
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

struct HidHandle(HANDLE);
impl Drop for HidHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn hid_paths() -> Result<BTreeSet<String>> {
    unsafe {
        let set = DeviceSet(SetupDiGetClassDevsW(
            Some(&HID_INTERFACE),
            PCWSTR::null(),
            None,
            DIGCF_DEVICEINTERFACE | DIGCF_PRESENT,
        )?);
        let mut paths = BTreeSet::new();
        for index in 0.. {
            let mut interface = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            if let Err(error) =
                SetupDiEnumDeviceInterfaces(set.0, None, &HID_INTERFACE, index, &mut interface)
            {
                if error.code() == ERROR_NO_MORE_ITEMS.to_hresult() {
                    break;
                }
                return Err(error.into());
            }
            let mut size = 0;
            let _ =
                SetupDiGetDeviceInterfaceDetailW(set.0, &interface, None, 0, Some(&mut size), None);
            ensure!(
                (8..=65536).contains(&size),
                "invalid HID interface path size"
            );
            let mut data = vec![0u64; (size as usize).div_ceil(8)];
            let detail = data.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            SetupDiGetDeviceInterfaceDetailW(set.0, &interface, Some(detail), size, None, None)?;
            paths.insert(
                PCWSTR((*detail).DevicePath.as_ptr())
                    .to_string()?
                    .to_lowercase(),
            );
        }
        Ok(paths)
    }
}

fn new_hid(
    before: &BTreeSet<String>,
    product: u16,
    library: &libloading::Library,
) -> Result<(String, HidHandle)> {
    #[repr(C)]
    struct Attributes {
        size: u32,
        vendor: u16,
        product: u16,
        version: u16,
    }
    type GetAttributes = unsafe extern "system" fn(HANDLE, *mut Attributes) -> u8;
    let get: libloading::Symbol<GetAttributes> = unsafe { library.get(b"HidD_GetAttributes\0")? };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let paths = hid_paths()?;
        let mut candidates = Vec::new();
        // Device interface names are opaque, including VHF paths which need
        // not contain the USB VID/PID spelling. Identify via HID attributes.
        for path in paths.difference(before) {
            let wide: Vec<_> = path.encode_utf16().chain([0]).collect();
            // GetInputReport needs a handle, not write access to any HID output.
            let Ok(handle) = (unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    GENERIC_READ.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            }) else {
                continue;
            };
            let handle = HidHandle(handle);
            let mut attributes = Attributes {
                size: size_of::<Attributes>() as u32,
                vendor: 0,
                product: 0,
                version: 0,
            };
            if unsafe { get(handle.0, &mut attributes) } != 0
                && attributes.vendor == 0x054c
                && attributes.product == product
            {
                candidates.push((path.clone(), handle));
            }
        }
        ensure!(
            candidates.len() <= 1,
            "more than one newly enumerated target HID"
        );
        if let Some(candidate) = candidates.pop() {
            return Ok(candidate);
        }
        ensure!(
            Instant::now() < deadline,
            "owned controller HID did not enumerate with VID 054c / PID {product:04x}; newly enumerated paths: {:?}",
            paths.difference(before).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Contact {
    active: bool,
    tracking: u8,
    x: u16,
    y: u16,
}

fn contacts(hid: &HidHandle, library: &libloading::Library, profile: u16) -> Result<[Contact; 2]> {
    type GetInputReport = unsafe extern "system" fn(HANDLE, *mut std::ffi::c_void, u32) -> u8;
    let get: libloading::Symbol<GetInputReport> = unsafe { library.get(b"HidD_GetInputReport\0")? };
    let mut report = [0u8; 64];
    report[0] = 1;
    if unsafe { get(hid.0, report.as_mut_ptr().cast(), report.len() as u32) } == 0 {
        return Err(windows::core::Error::from_thread().into());
    }
    ensure!(report[0] == 1, "unexpected HID report ID");
    // Native USB layouts, including the report ID: DS4 first touch packet at
    // byte 34 (one timestamp then two contacts); DualSense contacts at byte 33.
    // See libvirtualgamepad beta.6 driver/src/{dualshock4,dualsense}.h.
    let offset = if profile == 5 { 35 } else { 33 };
    Ok(std::array::from_fn(|index| {
        let b = &report[offset + index * 4..][..4];
        Contact {
            active: b[0] & 0x80 == 0,
            tracking: b[0] & 0x7f,
            x: u16::from(b[1]) | (u16::from(b[2] & 0x0f) << 8),
            y: (u16::from(b[2]) >> 4) | (u16::from(b[3]) << 4),
        }
    }))
}

fn send(pads: &mut Gamepads, touchpad: u8, event: u8, pointer: u32, x: f32, y: f32) -> Result<()> {
    // Moonlight 6.2.0 controller-touch packet, decoded through the production
    // parser before the production Windows mapping and IOCTL submission.
    let mut packet = [0u8; 28];
    packet[..4].copy_from_slice(&24u32.to_be_bytes());
    packet[4..8].copy_from_slice(&0x55000005u32.to_le_bytes());
    packet[8] = 15;
    packet[9] = event;
    packet[11] = touchpad;
    packet[12..16].copy_from_slice(&pointer.to_le_bytes());
    packet[16..20].copy_from_slice(&x.to_le_bytes());
    packet[20..24].copy_from_slice(&y.to_le_bytes());
    packet[24..28].copy_from_slice(&1f32.to_le_bytes());
    pads.apply(&butterpollo_core::input::decode(&packet)?)
}

#[test]
#[ignore = "creates temporary neutral VHF controllers; requires the installed signed driver and a coordinated idle input session"]
fn primary_multitouch_and_secondary_isolation_match_native_hid_reports() -> Result<()> {
    // Absolute system path avoids loading a lookalike DLL from the working dir.
    let system = std::env::var("SystemRoot").context("SystemRoot is missing")?;
    let library = unsafe { libloading::Library::new(format!("{system}\\System32\\hid.dll"))? };
    for (profile, product, height) in [(5, 0x09cc, 942u16), (6, 0x0ce6, 1080)] {
        let before = hid_paths()?;
        let mut pads = Gamepads::open(profile)?;
        // The client advertises two surfaces, but the virtual device only has
        // one. Explicit profile selection exercises both supported HID layouts.
        pads.apply(&Event::Arrival {
            id: 15,
            kind: 4,
            capabilities: 0x108,
            buttons: 0,
        })?;
        let (path, hid) = new_hid(&before, product, &library)?;
        assert!(
            contacts(&hid, &library, profile)?
                .iter()
                .all(|point| !point.active)
        );

        send(&mut pads, 0, 1, 100, 0.0, 1.0)?;
        send(&mut pads, 0, 1, 200, 1.0, 0.0)?;
        let two = contacts(&hid, &library, profile)?;
        assert!(two.iter().all(|point| point.active));
        assert_ne!(two[0].tracking, two[1].tracking);
        assert_eq!((two[0].x, two[0].y), (0, height - 1));
        assert_eq!((two[1].x, two[1].y), (1919, 0));

        // Same pointer IDs on another surface must not move, release, or
        // cancel either primary contact in the actual Windows report.
        for event in [1, 3, 2, 4, 6, 7] {
            send(&mut pads, 1, event, 100, 0.5, 0.5)?;
            assert_eq!(contacts(&hid, &library, profile)?, two);
        }
        // A third primary contact cannot overwrite either of the two slots.
        send(&mut pads, 0, 1, 300, 0.5, 0.5)?;
        assert_eq!(contacts(&hid, &library, profile)?, two);
        send(&mut pads, 0, 3, 100, 1.0, 1.0)?;
        let moved = contacts(&hid, &library, profile)?;
        assert_eq!(moved[0].tracking, two[0].tracking);
        assert_eq!((moved[0].x, moved[0].y), (1919, height - 1));
        assert_eq!(moved[1], two[1]);

        send(&mut pads, 0, 2, 100, 1.0, 1.0)?;
        let released = contacts(&hid, &library, profile)?;
        assert!(!released[0].active);
        assert_eq!(released[1], two[1]);
        send(&mut pads, 0, 1, 300, 0.0, 0.0)?;
        let reused = contacts(&hid, &library, profile)?;
        assert!(reused[0].active);
        assert_ne!(reused[0].tracking, two[0].tracking);
        assert_eq!((reused[0].x, reused[0].y), (0, 0));
        assert_eq!(reused[1], two[1]);
        send(&mut pads, 0, 7, 0, 0.0, 0.0)?;
        assert!(
            contacts(&hid, &library, profile)?
                .iter()
                .all(|point| !point.active)
        );

        drop(hid);
        drop(pads);
        let deadline = Instant::now() + Duration::from_secs(5);
        while hid_paths()?.contains(&path) {
            ensure!(
                Instant::now() < deadline,
                "owned HID remained after controller drop"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        println!(
            "profile={profile}: two contacts, secondary isolation, move/release/reuse/cancel, and removal verified"
        );
    }
    Ok(())
}
