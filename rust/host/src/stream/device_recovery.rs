use anyhow::Result;
use butterpollo_windows::device_loss::DeviceLost;
use std::time::{Duration, Instant};

pub(super) const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct DeviceRecovery {
    incident: Option<(DeviceLost, Instant)>,
    pub restart: bool,
    pub captured: bool,
}
impl DeviceRecovery {
    pub fn active(&self) -> bool {
        self.incident.is_some()
    }
    pub fn started(&self) -> Option<Instant> {
        self.incident.map(|(_, since)| since)
    }
    pub fn lost(&mut self, loss: DeviceLost, now: Instant) -> bool {
        self.restart = true;
        self.captured = false;
        if self.incident.is_some() {
            return false;
        }
        self.incident = Some((loss, now));
        true
    }
    pub fn check(&self, now: Instant) -> Result<()> {
        if let Some((loss, since)) = self.incident
            && now.duration_since(since) >= TIMEOUT
        {
            return Err(anyhow::Error::new(loss).context("The GPU did not recover within 30 seconds; the stream has ended. Check Device Manager; if the adapter remains unavailable (Code 31), reboot Windows before reconnecting. The host console and /serverinfo remain available."));
        }
        Ok(())
    }
    pub fn recovered(&mut self) -> bool {
        if self.captured && !self.restart {
            return self.incident.take().is_some();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_removal_and_reopening_do_not_extend_the_deadline() -> Result<()> {
        let now = Instant::now();
        let loss = DeviceLost(0x887a0005_u32 as i32);
        let mut recovery = DeviceRecovery::default();
        recovery.check(now)?;
        assert!(recovery.lost(loss, now));
        assert!(!recovery.recovered());
        recovery.restart = false;
        recovery.captured = true;
        assert!(!recovery.lost(loss, now + Duration::from_secs(29)));
        recovery.check(now + TIMEOUT - Duration::from_nanos(1))?;
        let error = recovery.check(now + TIMEOUT).unwrap_err();
        assert_eq!(DeviceLost::from_error(&error), Some(loss));
        assert!(error.to_string().contains("Code 31"));
        Ok(())
    }

    #[test]
    fn only_output_after_new_capture_finishes_an_incident() {
        let now = Instant::now();
        let loss = DeviceLost(0x887a0007_u32 as i32);
        let mut recovery = DeviceRecovery::default();
        assert!(recovery.lost(loss, now));
        assert!(!recovery.recovered());
        recovery.restart = false;
        assert!(!recovery.recovered());
        recovery.captured = true;
        assert!(recovery.recovered());
        assert!(!recovery.recovered());
        recovery.check(now + TIMEOUT).unwrap();
        assert!(recovery.lost(loss, now + TIMEOUT));
    }
}
