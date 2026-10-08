use super::*;
use crate::input::{Gamepads, SLOTS, gamepad_backend::Backend};
use butterpollo_core::{input::Input, input_policy::VIGEM_X360};

#[test]
fn missing_usb_read_does_not_disconnect_a_ready_target() {
    let failure =
        |code: WIN32_ERROR| Err(windows::core::Error::from_hresult(code.to_hresult()).into());
    assert!(report_result(Ok(())).is_ok());
    assert!(report_result(failure(ERROR_NO_MORE_ITEMS)).is_ok());
    assert!(report_result(failure(ERROR_ACCESS_DENIED)).is_err());
    assert!(report_result(failure(ERROR_INVALID_PARAMETER)).is_err());
}

#[test]
fn failed_unplug_retains_the_targets_feedback() -> Result<()> {
    let bus = INVALID_HANDLE_VALUE;
    let now = Instant::now();
    let mut target = Target {
        bus,
        serial: 1,
        ds4: None,
        notification: Some(Notification {
            bus,
            event: event()?,
            pending: Box::new(Pending {
                overlapped: OVERLAPPED::default(),
                data: [0; 16],
            }),
            running: false,
            ds4: false,
        }),
        last_report: now,
        last_gyro: now,
    };
    assert!(target.unplug().is_err());
    assert_eq!(target.serial, 1);
    assert!(target.notification.is_some());
    Ok(())
}

#[test]
fn wire_headers_match_bus_shared_h() {
    assert_eq!(PLUGIN, 0x2aa004);
    assert_eq!(WAIT_READY, 0x2aa010);
    assert_eq!(XUSB_SUBMIT, 0x2aa808);
    assert_eq!(DS4_SUBMIT, 0x2aa80c);
    assert_eq!(XUSB_NOTIFICATION, 0x2ae804);
    // BUSENUM_W_IOCTL, unlike the XUSB notification's BUSENUM_RW_IOCTL.
    assert_eq!(DS4_NOTIFICATION, 0x2aa810);
    assert_eq!(packet(8, 0x12345678), [8, 0, 0, 0, 0x78, 0x56, 0x34, 0x12]);
    assert_eq!(Ds4Report::new().0.len() + 8, 71);
}

#[test]
fn xusb_buttons_axes_and_triggers_match_vibepollo() {
    assert_eq!(xusb_report(0, 0, 0, &[0; 4]), [0; 12]);
    for bit in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13, 14, 15] {
        let report = xusb_report(1 << bit, 0, 0, &[0; 4]);
        assert_eq!(u16::from_le_bytes([report[0], report[1]]), 1 << bit);
    }
    assert_eq!(xusb_report(0x200000, 0, 0, &[0; 4])[..2], [0, 4]);
    assert_eq!(xusb_report(0xffff0800, 0, 0, &[0; 4])[..2], [0, 4]);
    assert_eq!(
        xusb_report(0x9231, 17, 255, &[i16::MIN, i16::MAX, -1, 0x1234]),
        [0x31, 0x92, 17, 255, 0, 128, 255, 127, 255, 255, 0x34, 0x12]
    );
}

#[test]
fn ds4_dpad_buttons_and_axes_match_vibepollo() {
    let mut report = Ds4Report::new();
    assert_eq!(&report.0[..9], &[128, 128, 128, 128, 8, 0, 0, 0, 0]);
    for (flags, hat) in [
        (0, 8),
        (1, 0),
        (9, 1),
        (8, 2),
        (10, 3),
        (2, 4),
        (6, 5),
        (4, 6),
        (5, 7),
        (15, 1),
    ] {
        report.state(flags, 0, 0, &[0; 4]);
        assert_eq!(report.0[4] & 15, hat);
    }
    for (flags, buttons) in [
        (0x40, 0x4000u16),
        (0x80, 0x8000),
        (0x100, 0x100),
        (0x200, 0x200),
        (0x10, 0x2000),
        (0x20, 0x1000),
        (0x1000, 0x20),
        (0x2000, 0x40),
        (0x4000, 0x10),
        (0x8000, 0x80),
    ] {
        report.state(flags, 0, 0, &[0; 4]);
        assert_eq!(
            u16::from_le_bytes(report.0[4..6].try_into().unwrap()),
            buttons | 8
        );
    }
    report.state(0x300400, 1, 254, &[i16::MIN, i16::MAX, i16::MAX, i16::MIN]);
    assert_eq!(&report.0[..9], &[0, 2, 255, 255, 8, 12, 3, 1, 254]);
    report.state(0, 0, 0, &[0; 4]);
    assert_eq!(&report.0[..9], &[127, 129, 127, 129, 8, 0, 0, 0, 0]);
}

#[test]
fn ds4_touch_packs_two_contacts_and_retains_tracking_until_release() {
    let mut report = Ds4Report::new();
    report.touch(0, 1, [0.5, 1.]);
    assert_eq!(
        &report.0[32..42],
        &[1, 1, 1, 0xc0, 0xf3, 0x3a, 0x80, 0, 0, 0]
    );
    report.touch(1, 1, [1., 0.]);
    assert_eq!(&report.0[38..42], &[1, 0x80, 7, 0]);
    report.touch(0, 3, [0., 0.]);
    assert_eq!(&report.0[34..38], &[1, 0, 0, 0]);
    report.touch(0, 2, [0., 0.]);
    assert_eq!(report.0[34], 0x81);
    report.touch(0, 1, [0., 0.]);
    assert_eq!(report.0[34], 2);
    report.touch(0, 5, [0., 0.]);
    assert_eq!(report.0[34], 0x82);
    assert_eq!(report.0[38], 0x81);
    assert_eq!(report.0[33], 6);
    for _ in 0..130 {
        report.touch(0, 1, [0., 0.]);
        assert_eq!(report.0[34] & 0x80, 0);
        report.touch(0, 4, [0., 0.]);
    }
}

#[test]
fn ds4_motion_uses_vigem_calibration_and_preserves_other_fields() {
    let mut report = Ds4Report::new();
    report.touch(0, 1, [0.25, 0.5]);
    report.motion(1, &[9.80665, -9.80665, 0.]);
    report.motion(2, &[90., -180., 0.]);
    let axes = |offset| -> [i16; 3] {
        std::array::from_fn(|axis| {
            i16::from_le_bytes(
                report.0[offset + axis * 2..offset + axis * 2 + 2]
                    .try_into()
                    .unwrap(),
            )
        })
    };
    // Golden values from Vibepollo's MPS2_TO_DS4_ACCEL / DPS_TO_DS4_GYRO
    // and APPLY_CALIBRATION, with float (not double) arithmetic.
    assert_eq!(axes(18), [7810, -8115, -499]);
    assert_eq!(axes(12), [1474, -2961, 0]);
    let extended = report.0[9..].to_vec();
    report.state(0x1000, 0, 0, &[0; 4]);
    assert_eq!(&report.0[9..], extended);
    report.motion(2, &[40000., -40000., f32::NAN]);
    assert_eq!(&report.0[12..18], &[255, 127, 0, 128, 0, 0]);
    report.advance_timestamp(Duration::from_nanos(5333 * 65535));
    report.advance_timestamp(Duration::from_nanos(5333 * 2));
    assert_eq!(&report.0[9..11], &[1, 0]);
}

#[test]
fn ds4_battery_clamps_out_of_range_client_percentages() {
    for percent in [251, 101, 254] {
        let mut report = Ds4Report::new();
        report.battery(3, percent);
        assert_eq!((report.0[11], report.0[29]), (255, 0x1a));
    }
}

#[test]
fn ds4_battery_scaling_and_unknown_values_match_vibepollo() {
    let mut report = Ds4Report::new();
    for (percent, scaled, level) in [
        (0, 0, 0x10),
        (50, 127, 0x15),
        (55, 140, 0x16),
        (100, 255, 0x1a),
    ] {
        report.battery(3, percent);
        assert_eq!((report.0[11], report.0[29]), (scaled, level));
    }
    report.battery(0, 255);
    assert_eq!((report.0[11], report.0[29]), (255, 0x1a));
    report.battery(4, 255);
    assert_eq!((report.0[11], report.0[29]), (255, 0x1b));
    report.battery(2, 50);
    assert_eq!((report.0[11], report.0[29]), (127, 5));
    for state in [1, 5] {
        report.battery(state, 25);
        assert_eq!((report.0[11], report.0[29]), (63, 0x1f));
    }
    report.battery(3, 255);
    assert_eq!(report.0[29], 0x15);
}

#[test]
fn notifications_use_the_existing_rumble_and_rgb_feedback_format() {
    assert_eq!(
        feedback_report(false, &[12, 0, 0, 0, 7, 0, 0, 0, 255, 128, 3, 0]),
        (3, vec![255, 255, 128, 128, 0, 0, 0, 0])
    );
    assert_eq!(
        feedback_report(
            true,
            &[16, 0, 0, 0, 7, 0, 0, 0, 128, 255, 12, 34, 56, 0, 0, 0]
        ),
        (1, vec![255, 255, 128, 128, 12, 34, 56, 0])
    );
}

fn require_idle_host() -> Result<()> {
    let log = std::fs::read_to_string(r"C:\ProgramData\Butterpollo\config\logs\butterpollo.log")
        .context("cannot establish that the installed host is idle")?;
    let mut connected = std::collections::BTreeSet::new();
    for line in log.lines() {
        if let Some(client) = line.split("CLIENT CONNECTED client=").nth(1) {
            connected.insert(client.split_whitespace().next().unwrap_or(client));
        } else if let Some(client) = line.split("CLIENT DISCONNECTED client=").nth(1) {
            connected.remove(client.split_whitespace().next().unwrap_or(client));
        }
    }
    if !connected.is_empty() {
        bail!("stream active; refusing to plug test controllers: {connected:?}");
    }
    Ok(())
}

#[test]
#[ignore = "briefly plugs neutral VHF and ViGEm DS4 pads on the worker; requires both drivers and an idle installed host"]
fn backend_open_does_not_block_input_and_worker_drop_frees_slots() -> Result<()> {
    use crate::input::{GamepadThread, Injector, PadReport};
    use butterpollo_core::input_policy::VHF_AUTO;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    static INJECTED: AtomicUsize = AtomicUsize::new(0);
    for (profile, backend) in [(VHF_AUTO, "VHF"), (0, "ViGEmBus+VHF")] {
        require_idle_host()?;
        let slots_before = *SLOTS.lock().unwrap();
        let (ready, opened) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let mut injector = Injector::new(r"\\.\DISPLAY99", "auto")?;
        injector.gamepads = GamepadThread::spawn(move || {
            let pads = Gamepads::open(profile)?;
            ready.send((std::thread::current().id(), pads.backend.name()))?;
            // Hold a real backend on its worker before open completes. Input
            // must finish while the control thread has not released it.
            resume.recv_timeout(Duration::from_secs(10))?;
            Ok(pads)
        })?;
        INJECTED.store(0, Ordering::Relaxed);
        injector.inject = |inputs| {
            INJECTED.fetch_add(inputs.len(), Ordering::Relaxed);
            inputs.len()
        };
        // An Xbox-type client with motion: a VHF DualSense on VHF, a ViGEm
        // DualShock 4 when ViGEmBus is also open.
        injector.apply(&Input::Arrival {
            id: 0,
            kind: 1,
            capabilities: 0x30,
            buttons: 0,
        })?;
        let (thread, selected) = opened.recv_timeout(Duration::from_secs(10))?;
        assert_ne!(thread, std::thread::current().id());
        assert_eq!(selected, backend);
        let events = [
            Input::Keyboard {
                key: 0x46,
                down: true,
                flags: 0,
                modifiers: 0,
            },
            Input::Keyboard {
                key: 0x46,
                down: false,
                flags: 0,
                modifiers: 0,
            },
            Input::Relative { x: 0, y: 0 },
        ];
        for _ in 0..100 {
            assert!(injector.apply_all(&events).is_empty());
        }
        assert_eq!(INJECTED.load(Ordering::Relaxed), 300);
        assert_eq!(*SLOTS.lock().unwrap(), slots_before);
        release.send(())?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !injector.gamepad_reports().any(|report| {
            matches!(
                report,
                PadReport::Motion {
                    id: 0,
                    capabilities: 0x30
                }
            )
        }) {
            require_idle_host()?;
            anyhow::ensure!(
                Instant::now() < deadline,
                "{backend} arrival was not reported"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_ne!(*SLOTS.lock().unwrap(), slots_before);
        std::thread::sleep(Duration::from_millis(24));
        require_idle_host()?;
        drop(injector);
        assert_eq!(*SLOTS.lock().unwrap(), slots_before);
        eprintln!(
            "{backend}: opened on {thread:?}; 100 keyboard/mouse passes completed while open was held; motion reply received and slots freed on worker drop"
        );
    }
    Ok(())
}

#[repr(C)]
#[derive(Default)]
struct XInputState {
    packet: u32,
    buttons: u16,
    triggers: [u8; 2],
    sticks: [i16; 4],
}
type GetState = unsafe extern "system" fn(u32, *mut XInputState) -> u32;
fn xinput_mask(get: &GetState) -> u8 {
    (0..4).fold(0, |mask, index| {
        mask | if unsafe { get(index, &mut XInputState::default()) } == 0 {
            1 << index
        } else {
            0
        }
    })
}

#[test]
#[ignore = "plugs one neutral X360 and DS4; requires ViGEmBus and an idle installed Butterpollo host"]
fn neutral_targets_enumerate_once_and_unplug_on_peer_drop() -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    require_idle_host()?;
    let system = std::env::var("SystemRoot")?;
    let library =
        unsafe { libloading::Library::new(format!("{system}\\System32\\xinput1_4.dll"))? };
    let get: libloading::Symbol<GetState> = unsafe { library.get(b"XInputGetState\0")? };
    let before = xinput_mask(&get);
    if before == 15 {
        bail!("all four XInput slots are occupied");
    }
    let steam_path = r"C:\games\steam\logs\controller.txt";
    let steam_offset = std::fs::metadata(steam_path)
        .map(|metadata| metadata.len())
        .ok();
    let slots_before = *SLOTS.lock().unwrap();
    let mut pads = Gamepads::open(0)?;
    if matches!(pads.backend, Backend::Vhf(_)) {
        bail!("ViGEmBus unavailable; refusing a VHF plug cycle");
    }
    require_idle_host()?;
    pads.apply(&Input::Arrival {
        id: 0,
        kind: 1,
        capabilities: 0,
        buttons: 0,
    })?;
    assert_eq!(pads.profiles[&0], VIGEM_X360);
    pads.apply(&Input::Controller {
        id: 0,
        active: 1,
        buttons: 0,
        left_trigger: 0,
        right_trigger: 0,
        sticks: [0; 4],
    })?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        require_idle_host()?;
        let _ = pads.feedback();
        std::thread::sleep(Duration::from_millis(100));
    }
    let after_x360 = xinput_mask(&get);
    assert_eq!(
        (after_x360 & !before).count_ones(),
        1,
        "X360 must occupy exactly one new XInput slot"
    );
    assert_eq!(after_x360 & before, before);
    let index = (after_x360 & !before).trailing_zeros();
    let mut state = XInputState::default();
    assert_eq!(unsafe { get(index, &mut state) }, 0);
    assert_eq!((state.buttons, state.triggers), (0, [0; 2]));
    // XusbPdo.cpp seeds enumeration with these axes, inside XInput deadzones.
    // Its zeroed report cache suppresses the first all-zero submission.
    if state.sticks != [0; 4] {
        assert_eq!(state.sticks, [-3356, -1869, -3255, -848]);
    }
    if let Some(client) = pads.backend.vigem() {
        let serial = client.targets[&u32::from(pads.active[&0])].serial;
        eprintln!(
            "X360 serial={serial}, XInput index={index}, masks {before:#x} -> {after_x360:#x}; zero buttons/triggers, sticks={:?} after neutral submission",
            state.sticks
        );
    }
    require_idle_host()?;
    // An Xbox-type client with motion and a touchpad (a Steam Deck) keeps its
    // ViGEm DualShock 4 when the VHF driver is open too.
    pads.apply(&Input::Arrival {
        id: 1,
        kind: 1,
        capabilities: 0x38,
        buttons: 0,
    })?;
    assert_eq!(pads.profiles[&1], VIGEM_DS4);
    assert_eq!(
        pads.backend.name_for(u32::from(pads.active[&1])),
        "ViGEmBus"
    );
    pads.apply(&Input::Controller {
        id: 1,
        active: 3,
        buttons: 0,
        left_trigger: 0,
        right_trigger: 0,
        sticks: [0; 4],
    })?;
    assert_ne!(pads.active[&0], pads.active[&1]);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        require_idle_host()?;
        pads.refresh()?;
        let _ = pads.feedback();
        std::thread::sleep(Duration::from_millis(100));
    }
    let owned_slots = pads.active.values().copied().collect::<Vec<_>>();
    drop(pads);
    assert_eq!(*SLOTS.lock().unwrap(), slots_before);
    let deadline = Instant::now() + Duration::from_secs(3);
    while xinput_mask(&get) != before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(xinput_mask(&get), before);
    eprintln!(
        "DS4 neutral reports accepted; dropping Gamepads released slots {owned_slots:?} and restored XInput mask {before:#x}"
    );
    if let Some(offset) = steam_offset {
        let mut file = std::fs::File::open(steam_path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut added = String::new();
        file.read_to_string(&mut added)?;
        let arrivals = added.matches("type: 045e 028e").count();
        eprintln!("Steam controller.txt: {arrivals} X360 arrival records during this test");
        for line in added.lines().filter(|line| {
            line.contains("Product:")
                || line.contains("type:")
                || line.contains("vid=0x045e, pid=0x028e")
        }) {
            eprintln!("{line}");
        }
        if !added.is_empty() {
            assert_eq!(arrivals, 1, "inspect Steam's new controller records");
        }
    }
    Ok(())
}

#[test]
#[ignore = "plugs one neutral X360 on ViGEmBus and one DualSense on VHF; requires both drivers and an idle installed Butterpollo host"]
fn automatic_puts_playstation_pads_on_vhf_and_xbox_pads_on_vigem() -> Result<()> {
    require_idle_host()?;
    let slots_before = *SLOTS.lock().unwrap();
    let mut pads = Gamepads::open(0)?;
    if !matches!(pads.backend, Backend::Mixed(_)) {
        bail!("needs both ViGEmBus and the VHF gamepad driver");
    }
    for (id, kind, profile, driver) in [(0, 1, VIGEM_X360, "ViGEmBus"), (1, 2, 6, "VHF")] {
        require_idle_host()?;
        pads.apply(&Input::Arrival {
            id,
            kind,
            capabilities: 0,
            buttons: 0,
        })?;
        assert_eq!(pads.profiles[&u16::from(id)], profile);
        let slot = u32::from(pads.active[&u16::from(id)]);
        assert_eq!(pads.backend.name_for(slot), driver);
    }
    pads.apply(&Input::Controller {
        id: 0,
        active: 3,
        buttons: 0,
        left_trigger: 0,
        right_trigger: 0,
        sticks: [0; 4],
    })?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        require_idle_host()?;
        pads.refresh()?;
        let _ = pads.feedback();
        std::thread::sleep(Duration::from_millis(100));
    }
    // Replugging a pad as the other client type moves it to the other driver.
    pads.apply(&Input::Arrival {
        id: 0,
        kind: 2,
        capabilities: 0,
        buttons: 0,
    })?;
    assert_eq!(pads.profiles[&0], 6);
    assert_eq!(pads.backend.name_for(u32::from(pads.active[&0])), "VHF");
    drop(pads);
    assert_eq!(*SLOTS.lock().unwrap(), slots_before);
    Ok(())
}

/// The bus's final status for `code` with serial 0. ViGEmBus pends every
/// request first, so an unknown code only fails once the request completes.
fn serial_zero_status(bus: HANDLE, code: u32, size: usize, output: bool) -> WIN32_ERROR {
    let event = event().unwrap();
    let mut overlapped = OVERLAPPED {
        hEvent: handle(&event),
        ..Default::default()
    };
    let mut data = packet(size, 0);
    let pointer = data.as_mut_ptr();
    let result = unsafe {
        DeviceIoControl(
            bus,
            code,
            Some(pointer.cast()),
            size as u32,
            output.then_some(pointer.cast()),
            if output { size as u32 } else { 0 },
            None,
            Some(&mut overlapped),
        )
    };
    let result = match result {
        Err(error) if error.code() == ERROR_IO_PENDING.to_hresult() => unsafe {
            if WaitForSingleObject(handle(&event), 1000) != WAIT_OBJECT_0 {
                let _ = CancelIoEx(bus, Some(&overlapped));
                let _ = GetOverlappedResult(bus, &overlapped, &mut 0, true);
                return ERROR_IO_PENDING;
            }
            GetOverlappedResult(bus, &overlapped, &mut 0, false)
        },
        other => other,
    };
    match result {
        Ok(()) => ERROR_SUCCESS,
        Err(error) => WIN32_ERROR::from_error(&error).unwrap(),
    }
}

#[test]
#[ignore = "opens the installed ViGEmBus; plugs nothing"]
fn installed_bus_routes_every_target_request() -> Result<()> {
    // Serial 0 is rejected by each handler with INVALID_PARAMETER before it
    // touches a target. A code missing from the bus's table fails with
    // NOT_SUPPORTED instead, and a notification with a wrong code still
    // reports IO_PENDING when it is issued.
    let bus = open_bus()?;
    for (name, code, size, output) in [
        ("WAIT_READY", WAIT_READY, 8, false),
        ("XUSB_SUBMIT", XUSB_SUBMIT, 20, false),
        ("DS4_SUBMIT", DS4_SUBMIT, 71, false),
        ("XUSB_NOTIFICATION", XUSB_NOTIFICATION, 12, true),
        ("DS4_NOTIFICATION", DS4_NOTIFICATION, 16, true),
    ] {
        assert_eq!(
            serial_zero_status(handle(&bus), code, size, output),
            ERROR_INVALID_PARAMETER,
            "{name} {code:#x}"
        );
    }
    Ok(())
}
