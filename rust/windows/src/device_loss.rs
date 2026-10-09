//! Typed device removal survives error context and the WGC helper boundary.
use windows::{
    Win32::Graphics::{Direct3D11::ID3D11Device, Direct3D12::ID3D12Device, Dxgi::*},
    core::HRESULT,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceLost(pub i32);

impl DeviceLost {
    pub fn from_code(code: HRESULT) -> Option<Self> {
        matches!(
            code,
            DXGI_ERROR_DEVICE_REMOVED
                | DXGI_ERROR_DEVICE_RESET
                | DXGI_ERROR_DEVICE_HUNG
                | DXGI_ERROR_DRIVER_INTERNAL_ERROR
        )
        .then_some(Self(code.0))
    }
    pub fn from_error(error: &anyhow::Error) -> Option<Self> {
        error.downcast_ref::<Self>().copied().or_else(|| {
            error
                .chain()
                .filter_map(|cause| cause.downcast_ref::<windows::core::Error>())
                .find_map(|error| Self::from_code(error.code()))
        })
    }
    pub fn d3d11(device: &ID3D11Device) -> Option<Self> {
        // SAFETY: This only queries the borrowed device; it does not wait for GPU work.
        unsafe {
            device
                .GetDeviceRemovedReason()
                .err()
                .map(|error| Self(error.code().0))
        }
    }
    pub fn d3d12(device: &ID3D12Device) -> Option<Self> {
        // SAFETY: This only queries the borrowed device; it does not wait for GPU work.
        unsafe {
            device
                .GetDeviceRemovedReason()
                .err()
                .map(|error| Self(error.code().0))
        }
    }
}
impl std::fmt::Display for DeviceLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match HRESULT(self.0) {
            DXGI_ERROR_DEVICE_REMOVED => "DXGI_ERROR_DEVICE_REMOVED",
            DXGI_ERROR_DEVICE_RESET => "DXGI_ERROR_DEVICE_RESET",
            DXGI_ERROR_DEVICE_HUNG => "DXGI_ERROR_DEVICE_HUNG",
            DXGI_ERROR_DRIVER_INTERNAL_ERROR => "DXGI_ERROR_DRIVER_INTERNAL_ERROR",
            _ => "GetDeviceRemovedReason",
        };
        write!(f, "GPU device lost: {name} (0x{:08X})", self.0 as u32)
    }
}
impl std::error::Error for DeviceLost {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_only_device_loss_and_preserves_context() {
        for code in [
            DXGI_ERROR_DEVICE_REMOVED,
            DXGI_ERROR_DEVICE_RESET,
            DXGI_ERROR_DEVICE_HUNG,
            DXGI_ERROR_DRIVER_INTERNAL_ERROR,
        ] {
            let error = anyhow::Error::from(windows::core::Error::from(code))
                .context("Map failed")
                .context("encoding failed");
            assert_eq!(DeviceLost::from_error(&error), Some(DeviceLost(code.0)));
        }
        for code in [
            HRESULT(0),
            DXGI_ERROR_ACCESS_LOST,
            DXGI_ERROR_WAIT_TIMEOUT,
            windows::Win32::Foundation::E_FAIL,
        ] {
            assert_eq!(DeviceLost::from_code(code), None);
        }
        assert_eq!(
            DeviceLost::from_error(&anyhow::anyhow!("DXGI_ERROR_DEVICE_REMOVED")),
            None
        );
        let loss = DeviceLost(DXGI_ERROR_DEVICE_HUNG.0);
        assert_eq!(
            DeviceLost::from_error(&anyhow::Error::new(loss).context("AMF")),
            Some(loss)
        );
        assert!(loss.to_string().contains("0x887A0006"));
    }
}
