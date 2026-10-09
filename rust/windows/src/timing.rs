use anyhow::Result;
use std::time::{Duration, Instant};
use windows::{
    Win32::{
        Foundation::*,
        System::{Power::*, Threading::*},
    },
    core::PCWSTR,
};

/// One high-resolution waitable timer per media worker. Packet pacing must not
/// pay the coarse scheduler tick for every UDP datagram.
pub struct Timer(HANDLE);
/// Each capture consumer owns a separate unnamed event. Resetting it cannot
/// consume another client's notification, and publishing before a wait is safe.
pub struct Signal(HANDLE);
/// Keep capture's display awake, as the C++ host does. The request belongs to
/// this thread and must be restored on the same thread, after capture ends.
pub struct DisplayAwake {
    previous: EXECUTION_STATE,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl DisplayAwake {
    pub fn enter() -> Result<Self> {
        let required = ES_CONTINUOUS | ES_DISPLAY_REQUIRED;
        // SAFETY: SetThreadExecutionState takes only flags by value and affects only this thread.
        let previous = unsafe { SetThreadExecutionState(required) };
        anyhow::ensure!(
            previous.0 != 0,
            "Windows rejected the display-awake request"
        );
        let guard = Self {
            previous,
            _thread: std::marker::PhantomData,
        };
        // Preserve any other requirement already owned by this thread, e.g.
        // a caller keeping the system awake. Drop also handles this error path.
        if previous & !required != EXECUTION_STATE(0) {
            anyhow::ensure!(
                // SAFETY: Flags only, by value; this changes only the calling thread's execution
                // state.
                unsafe { SetThreadExecutionState(previous | required) }.0 != 0,
                "Windows rejected the combined execution-state request"
            );
        }
        Ok(guard)
    }
}
impl Drop for DisplayAwake {
    fn drop(&mut self) {
        // SAFETY: Flags only; the guard is !Send, so this restores the state on the thread that set
        // it.
        unsafe {
            SetThreadExecutionState(self.previous | ES_CONTINUOUS);
        }
    }
}
// Windows event operations are thread-safe; ownership keeps the handle alive.
// SAFETY: Signal owns its event handle until Drop, and event calls may come from any thread.
unsafe impl Send for Signal {}
// SAFETY: SetEvent, ResetEvent and waits are thread-safe kernel calls, and none needs `&mut self`.
unsafe impl Sync for Signal {}
impl Signal {
    pub fn new() -> Result<Self> {
        // SAFETY: Null attributes and name create a new unnamed event; Signal owns the handle and
        // closes it only in Drop.
        Ok(Self(unsafe {
            CreateEventW(None, true, false, PCWSTR::null())?
        }))
    }
    pub fn set(&self) -> Result<()> {
        // SAFETY: `self.0` is the event this Signal owns, open until Drop.
        unsafe {
            SetEvent(self.0)?;
        }
        Ok(())
    }
    /// The event, to hand to another process.
    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }
    /// Reset while holding the protected capture-image lock, before waiting.
    pub fn reset(&self) -> Result<()> {
        // SAFETY: `self.0` is the event this Signal owns, open until Drop.
        unsafe {
            ResetEvent(self.0)?;
        }
        Ok(())
    }
}
impl Drop for Signal {
    fn drop(&mut self) {
        // SAFETY: `self.0` is owned by this Signal and closed exactly once, here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
impl Timer {
    pub fn new() -> Result<Self> {
        // SAFETY: Null attributes and name create a new unnamed timer; Timer owns the handle and
        // closes it only in Drop.
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
            // SAFETY: `self.0` is this Timer's open handle, `ticks` outlives the call, and no
            // completion routine is registered.
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
    /// Like `until`, accurate to a few microseconds at the cost of yielding the
    /// last 600 us. Waitable timers here wake 0.3-0.5 ms late for short waits,
    /// even at a 1 ms system timer resolution (`examples/timer_probe.rs`), so a
    /// paced packet burst would otherwise leave its last packet that much later.
    pub fn until_precise(&self, deadline: Instant) {
        const SPIN: Duration = Duration::from_micros(600);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining > SPIN + Duration::from_micros(100) {
            self.until(deadline - SPIN);
        }
        while Instant::now() < deadline {
            std::thread::yield_now();
        }
    }
    /// until_or_signal to the deadline itself: a timer wakes 0.13-0.47 ms
    /// late, so it is armed 0.6 ms early and the rest yields, still ending
    /// on the signal. For a frame's claim deadline, not for polls.
    pub fn until_or_signal_precise(&self, deadline: Instant, signal: &Signal) -> Result<bool> {
        const SPIN: Duration = Duration::from_micros(600);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining > SPIN + Duration::from_micros(100)
            && self.until_or_signal(deadline - SPIN, signal)?
        {
            return Ok(true);
        }
        while Instant::now() < deadline {
            // SAFETY: `signal.0` is an event the borrowed Signal keeps open; a zero timeout only
            // polls it.
            if unsafe { WaitForSingleObject(signal.0, 0) } == WAIT_OBJECT_0 {
                return Ok(true);
            }
            std::thread::yield_now();
        }
        Ok(false)
    }
    /// Wait for capture or the precise encoder/static-frame deadline, without
    /// a coarse condition-variable timeout or a polling/spinning thread.
    pub fn until_or_signal(&self, deadline: Instant, signal: &Signal) -> Result<bool> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        let ticks = -((remaining.as_nanos() / 100).clamp(1, i64::MAX as u128) as i64);
        // SAFETY: Both handles stay open for the borrows of `self` and `signal`, `ticks` outlives
        // the call, and no completion routine is registered.
        unsafe {
            SetWaitableTimer(self.0, &ticks, 0, None, None, false)?;
            let timeout = (remaining.as_millis() + 100).min(u128::from(u32::MAX - 1)) as u32;
            let result = WaitForMultipleObjects(&[self.0, signal.0], false, timeout);
            if result == WAIT_OBJECT_0 || result == WAIT_TIMEOUT {
                Ok(false)
            } else if result.0 == WAIT_OBJECT_0.0 + 1 {
                Ok(true)
            } else {
                Err(windows::core::Error::from_thread().into())
            }
        }
    }
}
impl Drop for Timer {
    fn drop(&mut self) {
        // SAFETY: `self.0` is owned by this Timer and closed exactly once, here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
/// Process-wide scheduling for the lifetime of at least one stream: a 1 ms
/// system timer, high priority class, and no power throttling. Without these,
/// driver waits (an AMF output query, a DXGI acquire) and Windows 11 timer
/// coalescing can add a scheduler tick to a frame. Like the C++ host it also
/// schedules the compositor under MMCSS, puts connected Wi-Fi adapters in
/// media streaming mode, and turns on Mouse Keys when no mouse is connected,
/// because Windows otherwise hides the cursor the stream should show.
/// Restored after the last stream.
pub struct StreamingScope(());
#[derive(Default)]
struct Streams {
    count: usize,
    priority: u32,
    /// The WLAN client handle that holds media streaming mode; closing it ends it.
    wlan: Option<usize>,
    /// Mouse Keys as they were before the first stream turned them on.
    mouse_keys: Option<windows::Win32::UI::Accessibility::MOUSEKEYS>,
}
static STREAMS: std::sync::Mutex<Streams> = std::sync::Mutex::new(Streams {
    count: 0,
    priority: 0,
    wlan: None,
    mouse_keys: None,
});
/// Mouse Keys on, when Windows has no mouse and would hide the cursor;
/// returns the previous settings to restore.
fn force_cursor() -> Option<windows::Win32::UI::Accessibility::MOUSEKEYS> {
    use windows::Win32::UI::{Accessibility::*, WindowsAndMessaging::*};
    // SAFETY: Both MOUSEKEYS are locals with cbSize set, and they outlive the SystemParametersInfoW
    // calls that read and write them.
    unsafe {
        if GetSystemMetrics(SM_MOUSEPRESENT) != 0 {
            return None;
        }
        let mut previous = MOUSEKEYS {
            cbSize: size_of::<MOUSEKEYS>() as u32,
            ..Default::default()
        };
        let mut enabled = MOUSEKEYS {
            cbSize: size_of::<MOUSEKEYS>() as u32,
            dwFlags: MKF_MOUSEKEYSON | MKF_AVAILABLE,
            iMaxSpeed: 10,
            iTimeToMaxSpeed: 1000,
            ..Default::default()
        };
        let result = SystemParametersInfoW(
            SPI_GETMOUSEKEYS,
            0,
            Some((&mut previous as *mut MOUSEKEYS).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .and_then(|()| {
            SystemParametersInfoW(
                SPI_SETMOUSEKEYS,
                0,
                Some((&mut enabled as *mut MOUSEKEYS).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        });
        match result {
            Ok(()) => {
                tracing::info!(
                    "no mouse connected; Mouse Keys are on while streaming so the cursor shows"
                );
                Some(previous)
            }
            Err(error) => {
                tracing::warn!(%error, "no mouse connected and Mouse Keys could not be turned on; the cursor may stay hidden");
                None
            }
        }
    }
}
/// Media streaming mode on every connected Wi-Fi adapter: fewer background
/// scans, which otherwise cause periodic loss and jitter. The returned WLAN
/// handle keeps it on until it is closed. wlanapi.dll is loaded at run time;
/// systems without it simply skip this.
fn wifi_streaming_mode() -> Option<usize> {
    use windows::Win32::{NetworkManagement::WiFi::*, System::LibraryLoader::*};
    type Open =
        unsafe extern "system" fn(u32, *const core::ffi::c_void, *mut u32, *mut HANDLE) -> u32;
    type Enumerate = unsafe extern "system" fn(
        HANDLE,
        *const core::ffi::c_void,
        *mut *mut WLAN_INTERFACE_INFO_LIST,
    ) -> u32;
    type Set = unsafe extern "system" fn(
        HANDLE,
        *const windows::core::GUID,
        WLAN_INTF_OPCODE,
        u32,
        *const core::ffi::c_void,
        *const core::ffi::c_void,
    ) -> u32;
    type Free = unsafe extern "system" fn(*const core::ffi::c_void);
    type Close = unsafe extern "system" fn(HANDLE, *const core::ffi::c_void) -> u32;
    // SAFETY: Each pointer is resolved from wlanapi.dll, never freed, with its documented
    // signature; `list` is non-null and freed only after its dwNumberOfItems entries are read.
    unsafe {
        let module = LoadLibraryW(windows::core::w!("wlanapi.dll")).ok()?;
        let open: Open =
            std::mem::transmute(GetProcAddress(module, windows::core::s!("WlanOpenHandle"))?);
        let enumerate: Enumerate = std::mem::transmute(GetProcAddress(
            module,
            windows::core::s!("WlanEnumInterfaces"),
        )?);
        let set: Set = std::mem::transmute(GetProcAddress(
            module,
            windows::core::s!("WlanSetInterface"),
        )?);
        let free: Free =
            std::mem::transmute(GetProcAddress(module, windows::core::s!("WlanFreeMemory"))?);
        let close: Close = std::mem::transmute(GetProcAddress(
            module,
            windows::core::s!("WlanCloseHandle"),
        )?);
        let (mut version, mut handle) = (0, HANDLE::default());
        if open(2, std::ptr::null(), &mut version, &mut handle) != 0 {
            return None;
        }
        let mut list = std::ptr::null_mut();
        if enumerate(handle, std::ptr::null(), &mut list) != 0 || list.is_null() {
            close(handle, std::ptr::null());
            return None;
        }
        let interfaces = std::slice::from_raw_parts(
            (&raw const (*list).InterfaceInfo).cast::<WLAN_INTERFACE_INFO>(),
            (*list).dwNumberOfItems as usize,
        );
        let enabled = windows::core::BOOL::from(true);
        let mut any = false;
        for interface in interfaces
            .iter()
            .filter(|i| i.isState == wlan_interface_state_connected)
        {
            if set(
                handle,
                &interface.InterfaceGuid,
                wlan_intf_opcode_media_streaming_mode,
                size_of::<windows::core::BOOL>() as u32,
                (&enabled as *const windows::core::BOOL).cast(),
                std::ptr::null(),
            ) == 0
            {
                any = true;
            }
        }
        free(list.cast());
        if !any {
            close(handle, std::ptr::null());
            return None;
        }
        tracing::info!("Wi-Fi adapter in media streaming mode while streaming");
        Some(handle.0 as usize)
    }
}
fn close_wlan(handle: usize) {
    use windows::Win32::System::LibraryLoader::*;
    type Close = unsafe extern "system" fn(HANDLE, *const core::ffi::c_void) -> u32;
    // SAFETY: wlanapi.dll was loaded and never freed, WlanCloseHandle has this signature, and
    // `handle` came from WlanOpenHandle; STREAMS hands it here only once.
    unsafe {
        if let Ok(module) = GetModuleHandleW(windows::core::w!("wlanapi.dll"))
            && let Some(close) = GetProcAddress(module, windows::core::s!("WlanCloseHandle"))
        {
            let close: Close = std::mem::transmute(close);
            close(HANDLE(handle as *mut _), std::ptr::null());
        }
    }
}
/// Windows 11 otherwise ignores timer requests from a process without a
/// visible window, and may run it on efficiency cores: the host, and the
/// capture helper in the user's session.
pub fn disable_power_throttling() {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    // SAFETY: `state` is an initialised local that outlives the call, the size passed is its size,
    // and GetCurrentProcess returns a pseudo handle.
    if let Err(error) = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    } {
        tracing::debug!(%error, "power throttling opt-out unavailable");
    }
}
impl StreamingScope {
    pub fn enter() -> Self {
        let mut streams = STREAMS.lock().unwrap();
        if streams.count == 0 {
            // SAFETY: DwmEnableMMCSS takes only a BOOL.
            unsafe {
                let _ = windows::Win32::Graphics::Dwm::DwmEnableMMCSS(true);
            }
            streams.wlan = wifi_streaming_mode();
            if streams.mouse_keys.is_none() {
                streams.mouse_keys = force_cursor();
            }
            // SAFETY: GetCurrentProcess returns a pseudo handle that needs no closing, and the
            // other calls take only plain values.
            unsafe {
                let process = GetCurrentProcess();
                disable_power_throttling();
                let _ = windows::Win32::Media::timeBeginPeriod(1);
                streams.priority = GetPriorityClass(process);
                if let Err(error) = SetPriorityClass(process, HIGH_PRIORITY_CLASS) {
                    tracing::debug!(%error, "high process priority unavailable");
                }
            }
        }
        streams.count += 1;
        Self(())
    }
}
impl Drop for StreamingScope {
    fn drop(&mut self) {
        let mut streams = STREAMS.lock().unwrap();
        streams.count -= 1;
        if streams.count == 0 {
            // SAFETY: The process pseudo handle needs no closing, the priority was saved by enter,
            // and the calls take only plain values.
            unsafe {
                let process = GetCurrentProcess();
                if streams.priority != 0 {
                    let _ = SetPriorityClass(process, PROCESS_CREATION_FLAGS(streams.priority));
                }
                let _ = windows::Win32::Media::timeEndPeriod(1);
                let _ = windows::Win32::Graphics::Dwm::DwmEnableMMCSS(false);
            }
            if let Some(handle) = streams.wlan.take() {
                close_wlan(handle);
            }
            // Keep the original settings until they are back, as the C++ host does.
            if let Some(mut previous) = streams.mouse_keys
                // SAFETY: `previous` is a local MOUSEKEYS, its cbSize set by force_cursor, that
                // outlives the call.
                && unsafe {
                    windows::Win32::UI::WindowsAndMessaging::SystemParametersInfoW(
                    windows::Win32::UI::WindowsAndMessaging::SPI_SETMOUSEKEYS,
                    0,
                    Some(
                        (&mut previous as *mut windows::Win32::UI::Accessibility::MOUSEKEYS).cast(),
                    ),
                    windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                )
                }
                .is_ok()
            {
                streams.mouse_keys = None;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "temporarily keeps the interactive display awake"]
    fn display_awake_preserves_and_restores_thread_power_requirements() -> Result<()> {
        // SAFETY: SetThreadExecutionState takes only flags and affects only this test's thread.
        let previous = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
        anyhow::ensure!(previous.0 != 0, "cannot set test execution state");
        let restore = DisplayAwake {
            previous,
            _thread: std::marker::PhantomData,
        };
        let before = ES_CONTINUOUS | ES_SYSTEM_REQUIRED;
        let required = before | ES_DISPLAY_REQUIRED;
        let first = DisplayAwake::enter()?;
        // SAFETY: SetThreadExecutionState takes only flags and affects only this test's thread.
        assert_eq!(unsafe { SetThreadExecutionState(required) }, required);
        let second = DisplayAwake::enter()?;
        drop(second);
        // SAFETY: SetThreadExecutionState takes only flags and affects only this test's thread.
        assert_eq!(unsafe { SetThreadExecutionState(required) }, required);
        drop(first);
        // SAFETY: SetThreadExecutionState takes only flags and affects only this test's thread.
        assert_eq!(unsafe { SetThreadExecutionState(before) }, before);
        drop(restore);
        Ok(())
    }
    #[test]
    #[ignore = "briefly puts connected Wi-Fi adapters in media streaming mode"]
    fn native_streaming_scope_is_restored_after_the_last_stream() {
        let first = StreamingScope::enter();
        let second = StreamingScope::enter();
        assert_eq!(STREAMS.lock().unwrap().count, 2);
        drop(second);
        drop(first);
        let streams = STREAMS.lock().unwrap();
        assert_eq!(streams.count, 0);
        assert!(streams.wlan.is_none());
        assert!(streams.mouse_keys.is_none());
    }
    #[test]
    fn capture_before_wait_is_retained_and_each_consumer_has_its_own_wake() -> Result<()> {
        let timer = Timer::new()?;
        let first = Signal::new()?;
        let second = Signal::new()?;
        first.set()?;
        second.set()?;
        first.reset()?;
        // One consumer resetting its event must leave the other ready.
        assert!(timer.until_or_signal(Instant::now() + Duration::from_secs(1), &second)?);
        let due = Instant::now() + Duration::from_millis(2);
        assert!(!timer.until_or_signal(due, &first)?);
        assert!(Instant::now() >= due);
        // Reusing the same timer must not retain its previous expiration.
        second.reset()?;
        let due = Instant::now() + Duration::from_millis(2);
        assert!(!timer.until_or_signal(due, &second)?);
        assert!(Instant::now() >= due);
        Ok(())
    }
    #[test]
    fn capture_can_interrupt_a_later_static_repeat_deadline() -> Result<()> {
        let timer = Timer::new()?;
        let signal = std::sync::Arc::new(Signal::new()?);
        let publisher = signal.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2));
            publisher.set().unwrap();
        });
        assert!(timer.until_or_signal(Instant::now() + Duration::from_secs(1), &signal)?);
        worker.join().unwrap();
        Ok(())
    }
}
