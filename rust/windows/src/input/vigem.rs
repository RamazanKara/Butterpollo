// ViGEmClient's DeviceIoControl protocol, ported to Rust from ViGEmClient.cpp,
// Common.h and km/BusShared.h (MIT). Copyright (c) 2016-2023 Nefarius Software
// Solutions e.U. and Contributors; see rust/THIRD_PARTY.md for the notice.
use anyhow::{Context, Result, bail};
use butterpollo_core::input_policy::VIGEM_DS4;
use std::{
    collections::BTreeMap,
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::*,
        Storage::FileSystem::*,
        System::{IO::*, Threading::*},
    },
    core::{GUID, PCWSTR},
};

const BUS: GUID = GUID::from_u128(0x96e42b22_f5e9_42f8_b043_ed0f932f014f);
const fn ioctl(function: u32, access: u32) -> u32 {
    (0x2a << 16) | (access << 14) | (function << 2)
}
const PLUGIN: u32 = ioctl(0x801, 2);
const UNPLUG: u32 = ioctl(0x802, 2);
const CHECK_VERSION: u32 = ioctl(0x803, 2);
const WAIT_READY: u32 = ioctl(0x804, 2);
const XUSB_NOTIFICATION: u32 = ioctl(0xa01, 3);
const XUSB_SUBMIT: u32 = ioctl(0xa02, 2);
const DS4_SUBMIT: u32 = ioctl(0xa03, 2);
const DS4_NOTIFICATION: u32 = ioctl(0xa04, 3);

fn handle(value: &OwnedHandle) -> HANDLE {
    HANDLE(value.as_raw_handle())
}
fn event() -> Result<OwnedHandle> {
    Ok(unsafe { OwnedHandle::from_raw_handle(CreateEventW(None, true, false, None)?.0) })
}
fn packet(size: usize, serial: u32) -> Vec<u8> {
    let mut data = vec![0; size];
    data[..4].copy_from_slice(&(size as u32).to_le_bytes());
    data[4..8].copy_from_slice(&serial.to_le_bytes());
    data
}

fn control(bus: HANDLE, code: u32, data: &[u8]) -> Result<()> {
    let event = event()?;
    let mut overlapped = OVERLAPPED {
        hEvent: handle(&event),
        ..Default::default()
    };
    let mut transferred = 0;
    unsafe {
        let started = DeviceIoControl(
            bus,
            code,
            Some(data.as_ptr().cast()),
            data.len() as u32,
            None,
            0,
            None,
            Some(&mut overlapped),
        );
        if let Err(error) = started
            && error.code() != ERROR_IO_PENDING.to_hresult()
        {
            return Err(error.into());
        }
        if WaitForSingleObject(handle(&event), 10_000) != WAIT_OBJECT_0 {
            let _ = CancelIoEx(bus, Some(&overlapped));
            // Cancellation is asynchronous: the kernel must release the input
            // buffer and OVERLAPPED before their storage goes away.
            let _ = GetOverlappedResult(bus, &overlapped, &mut transferred, true);
            bail!("ViGEmBus request {code:#x} timed out");
        }
        GetOverlappedResult(bus, &overlapped, &mut transferred, false)?;
    }
    Ok(())
}

fn report_result(result: Result<()>) -> Result<()> {
    match result {
        // A ready target may not have a USB read queued yet. ViGEmClient also
        // accepts this status; DS4's keepalive sends the cached report again.
        Err(error)
            if error
                .downcast_ref::<windows::core::Error>()
                .is_some_and(|error| error.code() == ERROR_NO_MORE_ITEMS.to_hresult()) =>
        {
            Ok(())
        }
        other => other,
    }
}

fn open_bus() -> Result<OwnedHandle> {
    unsafe {
        let set = SetupDiGetClassDevsW(
            Some(&BUS),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )?;
        let result = (|| {
            let mut last_error = anyhow::anyhow!("ViGEmBus is not installed");
            for index in 0.. {
                let mut interface = SP_DEVICE_INTERFACE_DATA {
                    cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                    ..Default::default()
                };
                match SetupDiEnumDeviceInterfaces(set, None, &BUS, index, &mut interface) {
                    Ok(()) => {}
                    Err(error) if error.code() == ERROR_NO_MORE_ITEMS.to_hresult() => break,
                    Err(error) => return Err(error.into()),
                }
                let opened = (|| -> Result<OwnedHandle> {
                    let mut size = 0;
                    let _ = SetupDiGetDeviceInterfaceDetailW(
                        set,
                        &interface,
                        None,
                        0,
                        Some(&mut size),
                        None,
                    );
                    if !(8..=65536).contains(&size) {
                        bail!("invalid ViGEmBus interface path length");
                    }
                    let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
                    let detail = buffer
                        .as_mut_ptr()
                        .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
                    (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
                    SetupDiGetDeviceInterfaceDetailW(
                        set,
                        &interface,
                        Some(detail),
                        size,
                        None,
                        None,
                    )?;
                    let bus = OwnedHandle::from_raw_handle(
                        CreateFileW(
                            PCWSTR((*detail).DevicePath.as_ptr()),
                            GENERIC_READ.0 | GENERIC_WRITE.0,
                            FILE_SHARE_READ | FILE_SHARE_WRITE,
                            None,
                            OPEN_EXISTING,
                            FILE_ATTRIBUTE_NORMAL
                                | FILE_FLAG_OVERLAPPED
                                | FILE_FLAG_NO_BUFFERING
                                | FILE_FLAG_WRITE_THROUGH,
                            None,
                        )?
                        .0,
                    );
                    control(handle(&bus), CHECK_VERSION, &packet(8, 1))
                        .context("incompatible ViGEmBus protocol")?;
                    Ok(bus)
                })();
                match opened {
                    Ok(bus) => return Ok(bus),
                    Err(error) => last_error = error,
                }
            }
            Err(last_error)
        })();
        let _ = SetupDiDestroyDeviceInfoList(set);
        result
    }
}

struct Pending {
    overlapped: OVERLAPPED,
    data: [u8; 16],
}
struct Notification {
    bus: HANDLE,
    event: OwnedHandle,
    // Gamepads can move between polls; pending kernel pointers must not move.
    pending: Box<Pending>,
    running: bool,
    ds4: bool,
}
impl Notification {
    fn new(bus: HANDLE, serial: u32, ds4: bool) -> Result<Self> {
        let event = event()?;
        let mut pending = Box::new(Pending {
            overlapped: OVERLAPPED::default(),
            data: [0; 16],
        });
        pending.data[..4].copy_from_slice(&(if ds4 { 16u32 } else { 12u32 }).to_le_bytes());
        pending.data[4..8].copy_from_slice(&serial.to_le_bytes());
        let mut notification = Self {
            bus,
            event,
            pending,
            running: false,
            ds4,
        };
        notification.start()?;
        Ok(notification)
    }
    fn start(&mut self) -> Result<()> {
        unsafe {
            ResetEvent(handle(&self.event))?;
            self.pending.overlapped = OVERLAPPED {
                hEvent: handle(&self.event),
                ..Default::default()
            };
            let size = if self.ds4 { 16 } else { 12 };
            let data = self.pending.data.as_mut_ptr();
            let result = DeviceIoControl(
                self.bus,
                if self.ds4 {
                    DS4_NOTIFICATION
                } else {
                    XUSB_NOTIFICATION
                },
                Some(data.cast()),
                size,
                Some(data.cast()),
                size,
                None,
                Some(&mut self.pending.overlapped),
            );
            if let Err(error) = result
                && error.code() != ERROR_IO_PENDING.to_hresult()
            {
                return Err(error.into());
            }
        }
        self.running = true;
        Ok(())
    }
    fn poll(&mut self) -> Result<Option<(u16, Vec<u8>)>> {
        if !self.running {
            self.start()?;
        }
        let mut transferred = 0;
        let result = unsafe {
            GetOverlappedResult(self.bus, &self.pending.overlapped, &mut transferred, false)
        };
        if let Err(error) = &result
            && error.code() == ERROR_IO_INCOMPLETE.to_hresult()
        {
            return Ok(None);
        }
        self.running = false;
        result?;
        if transferred != if self.ds4 { 16 } else { 12 } {
            bail!("invalid ViGEmBus notification length");
        }
        let report = feedback_report(self.ds4, &self.pending.data);
        self.start()?;
        Ok(Some(report))
    }
}
impl Drop for Notification {
    fn drop(&mut self) {
        if self.running {
            unsafe {
                let _ = CancelIoEx(self.bus, Some(&self.pending.overlapped));
                let _ = GetOverlappedResult(self.bus, &self.pending.overlapped, &mut 0, true);
            }
        }
    }
}

fn feedback_report(ds4: bool, data: &[u8]) -> (u16, Vec<u8>) {
    let (large, small) = if ds4 {
        (data[9], data[8])
    } else {
        (data[8], data[9])
    };
    let mut report = vec![0; 8];
    // Use the same 8-to-16 bit scaling as Vibepollo's ViGEm callbacks.
    report[..2].copy_from_slice(&(u16::from(large) * 257).to_le_bytes());
    report[2..4].copy_from_slice(&(u16::from(small) * 257).to_le_bytes());
    if ds4 {
        report[4..7].copy_from_slice(&data[10..13]);
    }
    (if ds4 { 1 } else { 3 }, report)
}

fn xusb_report(buttons: u32, left: u8, right: u8, sticks: &[i16; 4]) -> [u8; 12] {
    let mut report = [0; 12];
    let buttons = (buttons & 0xf7ff) as u16 | if buttons & 0x200000 != 0 { 0x400 } else { 0 };
    report[..2].copy_from_slice(&buttons.to_le_bytes());
    report[2..4].copy_from_slice(&[left, right]);
    for (offset, stick) in [4, 6, 8, 10].into_iter().zip(sticks) {
        report[offset..offset + 2].copy_from_slice(&stick.to_le_bytes());
    }
    report
}

#[derive(Clone)]
struct Ds4Report([u8; 63]);
impl Ds4Report {
    fn new() -> Self {
        let mut report = Self([0; 63]);
        report.0[..4].fill(0x80);
        report.0[4] = 8;
        report.0[11] = 0xff;
        report.0[29] = 0x1a;
        report.0[32] = 1;
        for offset in [34, 38, 43, 47, 52, 56] {
            report.0[offset] = 0x80;
        }
        report.motion(1, &[0., 9.80665, 0.]);
        report.motion(2, &[0.; 3]);
        report
    }
    fn state(&mut self, flags: u32, left: u8, right: u8, sticks: &[i16; 4]) {
        let mut buttons = match (
            flags & 1 != 0,
            flags & 2 != 0,
            flags & 8 != 0,
            flags & 4 != 0,
        ) {
            (true, _, true, _) => 1,
            (true, _, false, true) => 7,
            (true, _, false, false) => 0,
            (_, true, true, _) => 3,
            (_, true, false, true) => 5,
            (_, true, false, false) => 4,
            (_, _, true, _) => 2,
            (_, _, _, true) => 6,
            _ => 8,
        };
        for (source, target) in [
            (0x40, 0x4000),
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
            if flags & source != 0 {
                buttons |= target;
            }
        }
        if left != 0 {
            buttons |= 0x400;
        }
        if right != 0 {
            buttons |= 0x800;
        }
        self.0[4..6].copy_from_slice(&(buttons as u16).to_le_bytes());
        self.0[6] = u8::from(flags & 0x400 != 0) | (u8::from(flags & 0x300000 != 0) << 1);
        self.0[7..9].copy_from_slice(&[left, right]);
        for (i, stick) in sticks.iter().enumerate() {
            // Preserve Vibepollo's axis rounding, including its Y-axis inversion.
            self.0[i] = if i % 2 == 0 {
                ((i32::from(*stick) + 32768) / 257) as u8
            } else {
                let value = -(32766 + i32::from(*stick)) / 257;
                if value == 0 { 255 } else { value as u8 }
            };
        }
    }
    fn motion(&mut self, kind: u8, xyz: &[f32; 3]) {
        let (offset, limit, calibration) = match kind {
            1 => (
                18,
                200.,
                [(-297., 1.010796), (-42., 1.014614), (-512., 1.024768)],
            ),
            2 => (12, 4000., [(1., 0.977596), (0., 0.972370), (0., 0.971550)]),
            _ => return,
        };
        for (axis, (&value, (bias, scale))) in xyz.iter().zip(calibration).enumerate() {
            let value = if value.is_finite() {
                value.clamp(-limit, limit)
            } else {
                0.
            };
            let raw = if kind == 1 {
                value / 9.80665 * 8192.
            } else {
                value * 16.
            } as i32;
            // Games undo ViGEmBus's fixed factory calibration; apply its inverse.
            let calibrated = ((raw as f32 + bias) / scale) as i32;
            let value = calibrated.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            let offset = offset + axis * 2;
            self.0[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
    }
    fn touch(&mut self, slot: u8, event: u8, position: [f32; 2]) {
        let offset = 34 + usize::from(slot) * 4;
        match event {
            1 => {
                if self.0[offset] & 0x80 != 0 {
                    self.0[offset] = self.0[offset].wrapping_add(1) & 0x7f;
                }
            }
            2 | 4 => self.0[offset] |= 0x80,
            3 => {}
            5 => {
                self.0[34] |= 0x80;
                self.0[38] |= 0x80;
            }
            _ => return,
        }
        if event != 5 {
            let x = (position[0].clamp(0., 1.) * 1920.) as u16;
            let y = (position[1].clamp(0., 1.) * 943.) as u16;
            self.0[offset + 1..offset + 4].copy_from_slice(&[
                x as u8,
                ((x >> 8) | ((y & 15) << 4)) as u8,
                (y >> 4) as u8,
            ]);
        }
        self.0[33] = self.0[33].wrapping_add(1);
    }
    fn battery(&mut self, state: u8, percent: u8) {
        match state {
            2 | 3 => {
                if state == 3 {
                    self.0[29] |= 0x10;
                } else {
                    self.0[29] &= !0x10;
                }
                if self.0[29] & 15 > 10 {
                    self.0[29] = (self.0[29] & !15) | 5;
                }
            }
            4 => {
                self.0[29] = 0x1b;
                self.0[11] = 255;
            }
            1 | 5 => self.0[29] = 0x1f,
            _ => {}
        }
        if percent != 255 {
            self.0[11] = (u16::from(percent) * 255 / 100) as u8;
            if self.0[29] & 0x10 != 0 && self.0[29] & 15 <= 10 {
                self.0[29] = (self.0[29] & !15) | ((percent + 5) / 10);
            }
        }
    }
    fn advance_timestamp(&mut self, elapsed: Duration) {
        let previous = u16::from_le_bytes(self.0[9..11].try_into().unwrap());
        self.0[9..11].copy_from_slice(
            &previous
                .wrapping_add((elapsed.as_nanos() / 5333) as u16)
                .to_le_bytes(),
        );
    }
}

struct Target {
    bus: HANDLE,
    serial: u32,
    ds4: Option<Ds4Report>,
    notification: Option<Notification>,
    last_report: Instant,
    last_gyro: Instant,
}
impl Target {
    fn send_ds4(&mut self) -> Result<()> {
        let now = Instant::now();
        let report = self.ds4.as_mut().unwrap();
        if now.duration_since(self.last_gyro) > Duration::from_millis(750) {
            report.motion(2, &[0.; 3]);
        }
        report.advance_timestamp(now.duration_since(self.last_report));
        self.last_report = now;
        let mut data = packet(71, self.serial);
        data[8..].copy_from_slice(&report.0);
        report_result(control(self.bus, DS4_SUBMIT, &data))?;
        Ok(())
    }
    fn unplug(&mut self) -> Result<()> {
        self.notification = None;
        control(self.bus, UNPLUG, &packet(8, self.serial))?;
        self.serial = 0;
        Ok(())
    }
}
impl Drop for Target {
    fn drop(&mut self) {
        if self.serial != 0
            && let Err(error) = self.unplug()
        {
            tracing::warn!(%error, serial = self.serial, "could not unplug ViGEm target");
        }
    }
}

pub(super) struct Client {
    // Targets cancel their pending IO and unplug before the bus handle closes.
    targets: BTreeMap<u32, Target>,
    bus: OwnedHandle,
}
impl Client {
    pub(super) fn open() -> Result<Self> {
        Ok(Self {
            targets: BTreeMap::new(),
            bus: open_bus()?,
        })
    }
    pub(super) fn plug(&mut self, slot: u32, profile: u16) -> Result<()> {
        let ds4 = profile == VIGEM_DS4;
        let mut request = packet(16, 0);
        request[8..12].copy_from_slice(&(if ds4 { 2u32 } else { 0u32 }).to_le_bytes());
        let (vendor, product) = if ds4 {
            (0x054cu16, 0x05c4u16)
        } else {
            (0x045e, 0x028e)
        };
        request[12..14].copy_from_slice(&vendor.to_le_bytes());
        request[14..16].copy_from_slice(&product.to_le_bytes());
        let bus = handle(&self.bus);
        let mut last_error = anyhow::anyhow!("no free ViGEmBus serial numbers");
        for serial in 1..=u16::MAX as u32 {
            request[4..8].copy_from_slice(&serial.to_le_bytes());
            if let Err(error) = control(bus, PLUGIN, &request) {
                // Bus_PlugInDevice uses INVALID_PARAMETER for a serial collision.
                if !error
                    .downcast_ref::<windows::core::Error>()
                    .is_some_and(|error| error.code() == ERROR_INVALID_PARAMETER.to_hresult())
                {
                    return Err(error.context("could not plug ViGEm target"));
                }
                last_error = error;
                continue;
            }
            let now = Instant::now();
            let mut target = Target {
                bus,
                serial,
                ds4: ds4.then(Ds4Report::new),
                notification: None,
                last_report: now,
                last_gyro: now,
            };
            // Before v1.17 the plug request itself waited for readiness.
            if let Err(error) = control(bus, WAIT_READY, &packet(8, serial))
                && !error
                    .downcast_ref::<windows::core::Error>()
                    .is_some_and(|error| error.code() == ERROR_INVALID_PARAMETER.to_hresult())
            {
                return Err(error.context("ViGEm target did not become ready"));
            }
            target.notification = Some(Notification::new(bus, serial, ds4)?);
            if ds4 {
                target.send_ds4()?;
            }
            self.targets.insert(slot, target);
            return Ok(());
        }
        Err(last_error.context("could not plug ViGEm target"))
    }
    pub(super) fn unplug(&mut self, slot: u32) -> Result<()> {
        self.targets.get_mut(&slot).unwrap().unplug()?;
        self.targets.remove(&slot);
        Ok(())
    }
    pub(super) fn submit(
        &mut self,
        slot: u32,
        buttons: u32,
        left: u8,
        right: u8,
        sticks: &[i16; 4],
    ) -> Result<()> {
        let target = self.targets.get_mut(&slot).unwrap();
        if let Some(report) = &mut target.ds4 {
            report.state(buttons, left, right, sticks);
            target.send_ds4()
        } else {
            let mut data = packet(20, target.serial);
            data[8..].copy_from_slice(&xusb_report(buttons, left, right, sticks));
            report_result(control(target.bus, XUSB_SUBMIT, &data))?;
            Ok(())
        }
    }
    pub(super) fn motion(&mut self, slot: u32, kind: u8, xyz: &[f32; 3]) -> Result<()> {
        let target = self.targets.get_mut(&slot).unwrap();
        if let Some(report) = &mut target.ds4 {
            report.motion(kind, xyz);
            if kind == 2 {
                target.last_gyro = Instant::now();
            }
            target.send_ds4()?;
        }
        Ok(())
    }
    pub(super) fn touch(
        &mut self,
        slot: u32,
        contact: u8,
        event: u8,
        position: [f32; 2],
    ) -> Result<()> {
        let target = self.targets.get_mut(&slot).unwrap();
        if let Some(report) = &mut target.ds4 {
            let previous = report.clone();
            report.touch(contact, event, position);
            if let Err(error) = target.send_ds4() {
                target.ds4 = Some(previous);
                return Err(error);
            }
        }
        Ok(())
    }
    pub(super) fn battery(&mut self, slot: u32, state: u8, percent: u8) -> Result<()> {
        let target = self.targets.get_mut(&slot).unwrap();
        if let Some(report) = &mut target.ds4 {
            report.battery(state, percent);
            target.send_ds4()?;
        }
        Ok(())
    }
    pub(super) fn feedback(&mut self, slot: u32) -> Result<Option<(u16, Vec<u8>)>> {
        self.targets
            .get_mut(&slot)
            .unwrap()
            .notification
            .as_mut()
            .map(Notification::poll)
            .transpose()
            .map(Option::flatten)
    }
    pub(super) fn refresh(&mut self) -> Result<()> {
        for target in self.targets.values_mut() {
            if target.ds4.is_some() && target.last_report.elapsed() >= Duration::from_millis(100) {
                target.send_ds4()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "vigem_tests.rs"]
mod tests;
