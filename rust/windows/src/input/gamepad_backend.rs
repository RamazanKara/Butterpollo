use super::{GamepadTouchRequest, open_interface, request, vigem};
use anyhow::{Result, bail};
use butterpollo_core::input_policy::{
    GamepadBackend, VHF_AUTO, VIGEM_DS4, VIGEM_PROFILES, VIGEM_X360, gamepad_backend,
};
use windows::{
    Win32::{Foundation::*, System::IO::DeviceIoControl},
    core::GUID,
};

pub(super) enum Backend {
    Vhf(HANDLE),
    Vigem(vigem::Client),
}
impl Backend {
    pub(super) fn open(profile: u16) -> Result<(Self, u32)> {
        if matches!(profile, 0 | VIGEM_X360 | VIGEM_DS4) {
            let client = vigem::Client::open();
            if gamepad_backend(profile, client.is_ok()) == GamepadBackend::Vigem {
                return Ok((Self::Vigem(client?), VIGEM_PROFILES));
            }
            tracing::debug!(error = %client.err().unwrap(), "ViGEmBus unavailable; trying VHF gamepads");
        }
        let mut backend = Self::Vhf(open_interface(GUID::from_u128(
            0x27debbf5_1d1e_4e9c_906d_d104b1418b2b,
        ))?);
        let out = backend.ioctl(0x800, &request(8, None), 28)?;
        if out.len() != 28 || u16::from_le_bytes(out[4..6].try_into().unwrap()) != 2 {
            bail!("incompatible VHF gamepad protocol");
        }
        let available = u32::from_le_bytes(out[12..16].try_into().unwrap());
        if !matches!(profile, 0 | VHF_AUTO) && available & (1 << (profile - 1)) == 0 {
            bail!("configured controller profile is unavailable in the installed VHF driver");
        }
        Ok((backend, available))
    }
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Vhf(_) => "VHF",
            Self::Vigem(_) => "ViGEmBus",
        }
    }
    fn ioctl(&mut self, function: u32, data: &[u8], output: usize) -> Result<Vec<u8>> {
        let Self::Vhf(handle) = self else {
            unreachable!()
        };
        unsafe {
            let mut result = vec![0; output];
            let mut n = 0;
            DeviceIoControl(
                *handle,
                (0x22 << 16) | (3 << 14) | (function << 2),
                Some(data.as_ptr().cast()),
                data.len() as u32,
                (output != 0).then_some(result.as_mut_ptr().cast()),
                output as u32,
                Some(&mut n),
                None,
            )?;
            result.truncate(n as usize);
            Ok(result)
        }
    }
    pub(super) fn plug(&mut self, slot: u32, profile: u16) -> Result<()> {
        if let Self::Vigem(client) = self {
            return client.plug(slot, profile);
        }
        let mut b = request(16, Some(slot));
        b.extend_from_slice(&profile.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        self.ioctl(0x801, &b, 0)?;
        Ok(())
    }
    pub(super) fn unplug(&mut self, slot: u32) -> Result<()> {
        if let Self::Vigem(client) = self {
            return client.unplug(slot);
        }
        self.ioctl(0x802, &request(12, Some(slot)), 0)?;
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
        if let Self::Vigem(client) = self {
            return client.submit(slot, buttons, left, right, sticks);
        }
        let mut b = request(28, Some(slot));
        b.extend_from_slice(&buttons.to_le_bytes());
        for stick in sticks {
            b.extend_from_slice(&stick.to_le_bytes());
        }
        b.extend_from_slice(&[left, right, 0, 0]);
        self.ioctl(0x803, &b, 0)?;
        Ok(())
    }
    pub(super) fn motion(&mut self, slot: u32, kind: u8, xyz: &[f32; 3]) -> Result<()> {
        if let Self::Vigem(client) = self {
            return client.motion(slot, kind, xyz);
        }
        let mut b = request(28, Some(slot));
        b.extend_from_slice(&[kind, 0, 0, 0]);
        for f in xyz {
            b.extend_from_slice(
                &((*f as f64 * 1000.).clamp(i32::MIN as f64, i32::MAX as f64) as i32).to_le_bytes(),
            );
        }
        self.ioctl(0x806, &b, 0)?;
        Ok(())
    }
    pub(super) fn touch(&mut self, slot: u32, update: &GamepadTouchRequest) -> Result<()> {
        if let Self::Vigem(client) = self {
            return client.touch(slot, update.slot, update.event, update.position);
        }
        self.ioctl(0x805, &update.packet, 0)?;
        Ok(())
    }
    pub(super) fn battery(&mut self, slot: u32, state: u8, percent: u8) -> Result<()> {
        if let Self::Vigem(client) = self {
            return client.battery(slot, state, percent);
        }
        let mut b = request(16, Some(slot));
        b.extend_from_slice(&[percent, state, 0, 0]);
        self.ioctl(0x807, &b, 0)?;
        Ok(())
    }
    pub(super) fn feedback(&mut self, slot: u32) -> Result<Option<(u16, Vec<u8>)>> {
        if let Self::Vigem(client) = self {
            return client.feedback(slot);
        }
        let b = self.ioctl(0x804, &request(12, Some(slot)), 48)?;
        if b.len() == 48 {
            let kind = u16::from_le_bytes(b[12..14].try_into().unwrap());
            let len = u16::from_le_bytes(b[14..16].try_into().unwrap()) as usize;
            if kind != 0 && len <= 32 {
                return Ok(Some((kind, b[16..16 + len].to_vec())));
            }
        }
        Ok(None)
    }
    pub(super) fn refresh(&mut self) -> Result<()> {
        if let Self::Vigem(client) = self {
            client.refresh()?;
        }
        Ok(())
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        if let Self::Vhf(handle) = self {
            unsafe {
                let _ = CloseHandle(*handle);
            }
        }
    }
}
