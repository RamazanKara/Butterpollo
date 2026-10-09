use super::super::native_touch_tests::{hid_paths, new_hid, wait_removed};
use super::*;
use anyhow::{Context, ensure};
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::Mutex,
    time::{Duration, Instant},
};
use windows::Win32::Storage::FileSystem::WriteFile;

#[test]
fn only_an_empty_feedback_queue_is_not_a_poll_failure() {
    assert!(
        decode_feedback(Err(ERROR_NO_MORE_ITEMS.into()))
            .unwrap()
            .is_none()
    );
    for code in [
        ERROR_ACCESS_DENIED,
        ERROR_NOT_READY,
        ERROR_DEVICE_NOT_CONNECTED,
        ERROR_INVALID_PARAMETER,
        ERROR_INSUFFICIENT_BUFFER,
        ERROR_NOT_SUPPORTED,
    ] {
        let error = decode_feedback(Err(code.into())).unwrap_err();
        assert_eq!(
            error.downcast_ref::<windows::core::Error>().unwrap().code(),
            code.to_hresult()
        );
    }
}

#[test]
fn pending_feedback_preserves_the_driver_kind_and_strengths() {
    for (kind, size) in [(4u16, 8u16), (5, 32)] {
        let mut packet = request(48, Some(15));
        packet.extend_from_slice(&kind.to_le_bytes());
        packet.extend_from_slice(&size.to_le_bytes());
        packet.extend_from_slice(&[0x00, 0x80, 0x00, 0x40]);
        packet.resize(48, 0);
        let expected = packet[16..16 + usize::from(size)].to_vec();
        assert_eq!(decode_feedback(Ok(packet)).unwrap(), Some((kind, expected)));
    }
    assert_eq!(decode_feedback(Ok(vec![0; 48])).unwrap(), None);
}

static HARDWARE: Mutex<()> = Mutex::new(());
const SLOT: u32 = 15;

fn idle_host() -> Result<()> {
    let timeout = Duration::from_secs(2);
    let mut stream = TcpStream::connect_timeout(&"127.0.0.1:47989".parse()?, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream
        .write_all(b"GET /serverinfo HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    ensure!(response.starts_with("HTTP/1.1 200 "), "serverinfo failed");
    for name in ["RustHostSessionCount", "RustHostPendingSessionCount"] {
        let value = response
            .split_once(&format!("<{name}>"))
            .and_then(|(_, tail)| tail.split_once(&format!("</{name}>")))
            .map(|(value, _)| value);
        ensure!(
            value == Some("0"),
            "host is busy or {name} is missing: {value:?}"
        );
    }
    println!("serverinfo: sessions=0 pending=0");
    Ok(())
}

fn system_library(name: &str) -> Result<libloading::Library> {
    let system = std::env::var("SystemRoot").context("SystemRoot is missing")?;
    // SAFETY: Callers select Windows HID or XInput DLLs from System32; their loader
    // initialization/cleanup has no additional caller requirements, and Library owns the module.
    Ok(unsafe { libloading::Library::new(format!("{system}\\System32\\{name}"))? })
}

struct TestPad(Backend);
impl TestPad {
    fn plug(profile: u16) -> Result<Self> {
        idle_host()?;
        let (mut backend, _) = Backend::open(profile)?;
        // The driver refuses an occupied slot and ties ownership to this
        // handle. Never unplug or poll a slot unless our create succeeded.
        backend.plug(SLOT, profile)?;
        println!("created owned VHF profile={profile} slot={SLOT}");
        Ok(Self(backend))
    }
}
impl Drop for TestPad {
    fn drop(&mut self) {
        if let Err(error) = self.0.unplug(SLOT) {
            eprintln!("owned controller cleanup failed: {error:#}");
        }
    }
}

fn empty_queue(backend: &mut Backend) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while backend.feedback(SLOT)?.is_some() {
        ensure!(Instant::now() < deadline, "feedback queue did not drain");
    }
    let raw = backend.ioctl(0x804, &request(12, Some(SLOT)), 48);
    println!("0x804 empty: {raw:?}");
    assert_eq!(raw.unwrap_err().code(), ERROR_NO_MORE_ITEMS.to_hresult());
    assert_eq!(backend.feedback(SLOT)?, None);
    Ok(())
}

fn rumble(backend: &mut Backend, kind: u16, low: u16, high: u16) -> Result<()> {
    let expected = [low.to_le_bytes(), high.to_le_bytes()].concat();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut last = None;
    loop {
        if let Some((received_kind, data)) = backend.feedback(SLOT)? {
            println!("0x804 pending: kind={received_kind} payload={data:02x?}");
            if received_kind == kind && data.starts_with(&expected) {
                assert_eq!(data.len(), if kind == 4 { 8 } else { 32 });
                return Ok(());
            }
            last = Some((received_kind, data));
        }
        ensure!(
            Instant::now() < deadline,
            "no rumble kind={kind} low={low:#06x} high={high:#06x}; last={last:?}"
        );
        std::thread::sleep(Duration::from_millis(8));
    }
}

#[test]
#[ignore = "creates VHF Xbox controllers and writes XInput vibration; requires an idle installed host and no existing XInput pads"]
fn xbox_rumble_reaches_feedback_from_xinput() -> Result<()> {
    let _serial = HARDWARE.lock().unwrap();
    let library = system_library("xinput1_4.dll")?;
    type GetState = unsafe extern "system" fn(u32, *mut [u32; 4]) -> u32;
    type SetState = unsafe extern "system" fn(u32, *const [u16; 2]) -> u32;
    // SAFETY: The signature matches XInputGetState: [u32; 4] has XINPUT_STATE's
    // 16-byte size and 4-byte alignment, and library outlives the symbol.
    let get: libloading::Symbol<GetState> = unsafe { library.get(b"XInputGetState\0")? };
    // SAFETY: The signature matches XInputSetState and its two-u16 XINPUT_VIBRATION
    // input; library remains loaded for every use of the symbol.
    let set: libloading::Symbol<SetState> = unsafe { library.get(b"XInputSetState\0")? };
    let connected = || {
        (0..4)
            // SAFETY: The index is in XInput's 0..4 range and the aligned, initialized
            // output has room for the entire XINPUT_STATE for this synchronous call.
            .filter(|&id| unsafe { get(id, &mut [0; 4]) } == ERROR_SUCCESS.0)
            .collect::<Vec<_>>()
    };
    for profile in [4, 3] {
        idle_host()?;
        ensure!(
            connected().is_empty(),
            "existing XInput pads; refusing to write vibration"
        );
        let mut pad = TestPad::plug(profile)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let index = loop {
            let indices = connected();
            ensure!(indices.len() <= 1, "ambiguous new XInput controllers");
            if let Some(index) = indices.first() {
                break *index;
            }
            ensure!(
                Instant::now() < deadline,
                "owned Xbox pad did not reach XInput"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        println!("profile={profile} XInput index={index}");
        empty_queue(&mut pad.0)?;
        // Full and zero strengths survive the HID percentage quantization.
        for strengths in [[u16::MAX, 0], [0, u16::MAX], [0, 0]] {
            idle_host()?;
            // SAFETY: index came from the 0..4 enumeration, and strengths has the
            // initialized XINPUT_VIBRATION layout and lives through the call.
            assert_eq!(unsafe { set(index, &strengths) }, ERROR_SUCCESS.0);
            println!("XInputSetState: {strengths:?}");
            rumble(&mut pad.0, 4, strengths[0], strengths[1])?;
        }
        empty_queue(&mut pad.0)?;
        drop(pad);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !connected().is_empty() {
            ensure!(
                Instant::now() < deadline,
                "owned XInput controller remained after drop"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        println!("profile={profile}: XInput rumble, stop and removal verified");
    }
    Ok(())
}

#[test]
#[ignore = "creates VHF DS4, DualSense and Switch Pro controllers and writes HID output; requires an idle installed host"]
fn hid_rumble_reaches_feedback_for_each_profile() -> Result<()> {
    let _serial = HARDWARE.lock().unwrap();
    let library = system_library("hid.dll")?;
    for (profile, vendor, product, size) in [
        (5, 0x054c, 0x09cc, 32),
        (6, 0x054c, 0x0ce6, 48),
        (7, 0x057e, 0x2009, 64),
    ] {
        idle_host()?;
        let before = hid_paths()?;
        let mut pad = TestPad::plug(profile)?;
        let (path, hid) = new_hid(
            &before,
            vendor,
            product,
            GENERIC_READ.0 | GENERIC_WRITE.0,
            &library,
        )?;
        empty_queue(&mut pad.0)?;
        for running in [true, false] {
            let mut output = vec![0; size];
            let (low, high) = match profile {
                5 => {
                    output[0] = 0x05;
                    output[1] = 0x01;
                    output[4] = if running { 0x40 } else { 0 };
                    output[5] = if running { 0x80 } else { 0 };
                    (0x8000, 0x4000)
                }
                6 => {
                    output[0] = 0x02;
                    output[1] = 0x03;
                    output[3] = if running { 0x40 } else { 0 };
                    output[4] = if running { 0x80 } else { 0 };
                    (0x8000, 0x4000)
                }
                7 => {
                    output[0] = 0x10;
                    output[3] = if running { 200 } else { 0 };
                    output[5] = 0x40;
                    output[7] = if running { 100 } else { 0 };
                    output[9] = 0x40;
                    (65535, 32767)
                }
                _ => unreachable!(),
            };
            idle_host()?;
            let mut written = 0;
            // SAFETY: hid owns a synchronous write handle, and output and written
            // remain valid until WriteFile returns; the slice supplies its exact length.
            unsafe { WriteFile(hid.0, Some(&output), Some(&mut written), None)? };
            assert_eq!(written as usize, output.len());
            println!("profile={profile} HID WriteFile: {output:02x?}");
            rumble(
                &mut pad.0,
                5,
                if running { low } else { 0 },
                if running { high } else { 0 },
            )?;
        }
        empty_queue(&mut pad.0)?;
        drop(hid);
        drop(pad);
        wait_removed(&path)?;
        println!("profile={profile}: HID rumble, stop and removal verified");
    }
    Ok(())
}
