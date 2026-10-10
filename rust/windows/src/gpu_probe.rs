//! A trivial GPU round trip on a device of its own: an event query, which
//! completes once the GPU has run everything submitted before it. A stalled
//! GPU holds it back; a host thread stuck on its own work does not.
use crate::capture::Device;
use anyhow::{Context, Result};
use windows::{Win32::Graphics::Direct3D11::*, core::BOOL};

pub struct Probe {
    gpu: Device,
    query: ID3D11Query,
}
impl Probe {
    /// On the adapter showing `display` (the primary's if it is gone).
    pub fn new(display: &str) -> Result<Self> {
        let gpu = Device::new(display)?;
        let desc = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        };
        let mut query = None;
        // SAFETY: `desc` and `query` are live locals for the call.
        unsafe { gpu.device.CreateQuery(&desc, Some(&mut query))? };
        Ok(Self {
            gpu,
            query: query.context("D3D11 returned no event query")?,
        })
    }
    /// Submits the query to the GPU.
    pub fn issue(&self) {
        // SAFETY: the query belongs to this device; only this probe uses its context.
        unsafe {
            self.gpu.context.End(&self.query);
            self.gpu.context.Flush();
        }
    }
    /// Whether the GPU has reached the query issued last.
    pub fn done(&self) -> Result<bool> {
        let mut done = BOOL(0);
        // SAFETY: an event query writes one BOOL into the live local `done`,
        // and only when it has completed.
        unsafe {
            self.gpu.context.GetData(
                &self.query,
                Some((&raw mut done).cast()),
                size_of::<BOOL>() as u32,
                0,
            )?
        };
        Ok(done.as_bool())
    }
}
