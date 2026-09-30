use anyhow::Result;
use std::time::{Duration, Instant};
use windows::{
    Win32::{Foundation::*, System::Threading::*},
    core::PCWSTR,
};

/// One high-resolution waitable timer per media worker. Packet pacing must not
/// pay the coarse scheduler tick for every UDP datagram.
pub struct Timer(HANDLE);
impl Timer {
    pub fn new() -> Result<Self> {
        unsafe {
            let timer = CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
            .or_else(|_| CreateWaitableTimerExW(None, PCWSTR::null(), 0, TIMER_ALL_ACCESS.0))?;
            Ok(Self(timer))
        }
    }
    pub fn until(&self, deadline: Instant) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining > Duration::from_micros(100) {
            let ticks = -(((remaining - Duration::from_micros(50)).as_nanos() / 100)
                .min(i64::MAX as u128) as i64);
            unsafe {
                if SetWaitableTimer(self.0, &ticks, 0, None, None, false).is_ok() {
                    let _ = WaitForSingleObject(
                        self.0,
                        (remaining.as_millis() + 100).min(u32::MAX as u128) as u32,
                    );
                } else {
                    std::thread::sleep(remaining);
                }
            }
        }
        while Instant::now() < deadline {
            std::hint::spin_loop();
        }
    }
}
impl Drop for Timer {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
