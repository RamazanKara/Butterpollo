//! Owned DXGI ETW tracking. Failure keeps the original WGC composition time.

use crate::capture::qpc_frequency;
use butterpollo_core::present_timing::Refiner;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, OnceLock, Weak},
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{ERROR_INVALID_PARAMETER, ERROR_SUCCESS},
        Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW},
        System::{Diagnostics::Etw::*, Performance::QueryPerformanceCounter},
        UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
    },
    core::{GUID, PCWSTR, PWSTR},
};
const DXGI: GUID = GUID::from_u128(0xca11c036_0102_4a2d_a6ad_f03cfed5d3c9);
#[repr(C)]
struct Properties {
    header: EVENT_TRACE_PROPERTIES,
    name: [u16; 128],
}
impl Properties {
    fn new(fast: bool) -> Self {
        let mut s = Self {
            header: EVENT_TRACE_PROPERTIES::default(),
            name: [0; 128],
        };
        s.header.Wnode.BufferSize = size_of::<Self>() as u32;
        s.header.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        s.header.Wnode.ClientContext = 1;
        s.header.Wnode.Guid = GUID::from_u128(0x83be129b_0c78_445d_ba92_d60d0c2acae8);
        s.header.LogFileMode = EVENT_TRACE_REAL_TIME_MODE | if fast { 0x10 } else { 0 };
        s.header.FlushTimer = if fast { 2 } else { 1 };
        s.header.BufferSize = 16;
        s.header.MinimumBuffers = 4;
        s.header.MaximumBuffers = 32;
        s.header.LoggerNameOffset = std::mem::offset_of!(Self, name) as u32;
        s
    }
}
#[derive(Clone, Copy)]
struct Event {
    qpc: i64,
    pid: u32,
    chain: u64,
}
struct Events {
    values: Mutex<VecDeque<Event>>,
    retention: i64,
}
struct Tracker {
    session: CONTROLTRACE_HANDLE,
    consumer: PROCESSTRACE_HANDLE,
    worker: Option<thread::JoinHandle<()>>,
    // The callback points into this stable allocation, released after joining.
    events: Box<Events>,
}
impl Tracker {
    fn start() -> Option<Arc<Self>> {
        let mut s = Self {
            session: CONTROLTRACE_HANDLE::default(),
            consumer: PROCESSTRACE_HANDLE { Value: u64::MAX },
            worker: None,
            events: Box::new(Events {
                values: Mutex::new(VecDeque::new()),
                retention: qpc_frequency() * 2,
            }),
        };
        // Never stop or take over a trace belonging to another host instance.
        let name: Vec<u16> = format!("ButterpolloPresent-{}-{}", std::process::id(), hex_nonce())
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut properties = Properties::new(true);
        // SAFETY: `properties` is a repr(C) buffer whose BufferSize and LoggerNameOffset describe
        // it, and `name` is NUL-terminated; both outlive the call.
        let mut result = unsafe {
            StartTraceW(
                &mut s.session,
                PCWSTR(name.as_ptr()),
                &mut properties.header,
            )
        };
        if result == ERROR_INVALID_PARAMETER {
            properties = Properties::new(false);
            // SAFETY: As above, with freshly built `properties` and the same NUL-terminated `name`.
            result = unsafe {
                StartTraceW(
                    &mut s.session,
                    PCWSTR(name.as_ptr()),
                    &mut properties.header,
                )
            };
        }
        if result != ERROR_SUCCESS {
            s.session = CONTROLTRACE_HANDLE::default();
            tracing::info!(
                error = result.0,
                "DXGI present tracking unavailable; keeping capture timestamps"
            );
            return None;
        }
        // SAFETY: `s.session` was just started by StartTraceW, DXGI is a static GUID, and no filter
        // parameters are passed.
        let result = unsafe {
            EnableTraceEx2(
                s.session,
                &DXGI,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
                4,
                u64::MAX,
                0,
                0,
                None,
            )
        };
        if result != ERROR_SUCCESS {
            tracing::info!(
                error = result.0,
                "DXGI present provider unavailable; keeping capture timestamps"
            );
            return None;
        }
        let mut logfile = EVENT_TRACE_LOGFILEW {
            LoggerName: PWSTR(name.as_ptr().cast_mut()),
            Anonymous1: EVENT_TRACE_LOGFILEW_0 {
                ProcessTraceMode: PROCESS_TRACE_MODE_REAL_TIME
                    | PROCESS_TRACE_MODE_EVENT_RECORD
                    | PROCESS_TRACE_MODE_RAW_TIMESTAMP,
            },
            Anonymous2: EVENT_TRACE_LOGFILEW_1 {
                EventRecordCallback: Some(on_event),
            },
            Context: (&*s.events as *const Events).cast_mut().cast(),
            ..Default::default()
        };
        // SAFETY: `logfile` and the NUL-terminated `name` outlive the call; Context points into the
        // boxed Events, which Drop frees only after joining the trace worker.
        s.consumer = unsafe { OpenTraceW(&mut logfile) };
        if s.consumer.Value == u64::MAX {
            return None;
        }
        let handle = s.consumer.Value;
        match thread::Builder::new()
            .name("dxgi-presents".into())
            // SAFETY: `handle` was opened by OpenTraceW above and is closed only in Drop, which
            // then joins this worker before freeing the Events the callback reads.
            .spawn(move || unsafe {
                let _ = ProcessTrace(&[PROCESSTRACE_HANDLE { Value: handle }], None, None);
            }) {
            Ok(worker) => s.worker = Some(worker),
            Err(error) => {
                tracing::warn!(%error, "present tracking worker unavailable");
                return None;
            }
        }
        Some(Arc::new(s))
    }
    fn presents(&self, pid: u32, composition: i64, preferred: u64, output: &mut Vec<i64>) -> u64 {
        output.clear();
        let window = qpc_frequency();
        let values = self.events.values.lock().unwrap();
        let matching = values
            .iter()
            .filter(|e| e.pid == pid && e.qpc <= composition && e.qpc > composition - window);
        let selected = if preferred != 0
            && matching
                .clone()
                .any(|e| e.chain == preferred && e.qpc > composition - window / 4)
        {
            preferred
        } else {
            let mut counts = BTreeMap::<u64, usize>::new();
            for event in matching.clone() {
                *counts.entry(event.chain).or_default() += 1;
            }
            counts
                .into_iter()
                .max_by_key(|(_, count)| *count)
                .map_or(0, |(chain, _)| chain)
        };
        output.extend(
            matching
                .filter(|e| e.chain == selected && e.qpc > composition - window / 10)
                .map(|e| e.qpc),
        );
        output.sort_unstable();
        selected
    }
}
fn hex_nonce() -> u64 {
    u64::from_le_bytes(butterpollo_core::crypto::random::<8>())
}
unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    // SAFETY: ETW passes either null or a record that is valid for the whole callback.
    let Some(record) = (unsafe { record.as_ref() }) else {
        return;
    };
    let header = &record.EventHeader;
    if record.UserContext.is_null()
        || header.ProviderId != DXGI
        || !matches!(header.EventDescriptor.Id, 42 | 55)
    {
        return;
    }
    let pointer_bytes = if header.Flags & 0x20 != 0 { 4 } else { 8 };
    if record.UserData.is_null() || usize::from(record.UserDataLength) < pointer_bytes + 4 {
        return;
    }
    // SAFETY: UserData is non-null, and ETW guarantees UserDataLength bytes there for the callback.
    let data = unsafe {
        std::slice::from_raw_parts(
            record.UserData.cast::<u8>(),
            usize::from(record.UserDataLength),
        )
    };
    let mut chain = [0; 8];
    chain[..pointer_bytes].copy_from_slice(&data[..pointer_bytes]);
    let flags = u32::from_le_bytes(data[pointer_bytes..pointer_bytes + 4].try_into().unwrap());
    if flags & 1 != 0 {
        return;
    } // DXGI_PRESENT_TEST
    // SAFETY: UserContext is the non-null Context given to OpenTraceW, the boxed Events, which
    // outlives the consumer; the tests pass a live Events too.
    let events = unsafe { &*record.UserContext.cast::<Events>() };
    let Ok(mut values) = events.values.lock() else {
        return;
    };
    let qpc = header.TimeStamp;
    values.push_back(Event {
        qpc,
        pid: header.ProcessId,
        chain: u64::from_le_bytes(chain),
    });
    while values.len() > 16384
        || values
            .front()
            .is_some_and(|e| e.qpc < qpc - events.retention)
    {
        values.pop_front();
    }
}
impl Drop for Tracker {
    fn drop(&mut self) {
        if self.session.Value != 0 {
            let mut properties = Properties::new(false);
            // SAFETY: `self.session` was started by this Tracker and is stopped once, here;
            // `properties` is sized for the logger name the call writes back.
            unsafe {
                let _ = ControlTraceW(
                    self.session,
                    PCWSTR::null(),
                    &mut properties.header,
                    EVENT_TRACE_CONTROL_STOP,
                );
            }
        }
        if self.consumer.Value != u64::MAX {
            // SAFETY: `self.consumer` came from OpenTraceW and is closed only here.
            unsafe {
                let _ = CloseTrace(self.consumer);
            }
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn tracker() -> Option<Arc<Tracker>> {
    static SHARED: OnceLock<Mutex<Weak<Tracker>>> = OnceLock::new();
    let mut shared = SHARED
        .get_or_init(|| Mutex::new(Weak::new()))
        .lock()
        .unwrap();
    if let Some(active) = shared.upgrade() {
        return Some(active);
    }
    let active = Tracker::start()?;
    *shared = Arc::downgrade(&active);
    Some(active)
}
pub struct Stamper {
    tracker: Option<Arc<Tracker>>,
    output: String,
    grid: i64,
    next_poll: i64,
    pid: u32,
    chain: u64,
    presents: Vec<i64>,
    refiner: Refiner,
}
impl Default for Stamper {
    fn default() -> Self {
        Self {
            tracker: tracker(),
            output: String::new(),
            grid: 0,
            next_poll: 0,
            pid: 0,
            chain: 0,
            presents: Vec::new(),
            refiner: Refiner::default(),
        }
    }
}
impl Stamper {
    pub fn stamp(&mut self, captured: Instant, output: &str) -> Instant {
        if self.tracker.is_none() {
            return captured;
        }
        let frequency = qpc_frequency();
        if output != self.output {
            self.output = output.into();
            let name: Vec<u16> = output.encode_utf16().chain(Some(0)).collect();
            let mut mode = DEVMODEW {
                dmSize: size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            // SAFETY: `name` is NUL-terminated and `mode` has dmSize set; both outlive the call.
            self.grid = if unsafe {
                EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode)
            }
            .as_bool()
                && mode.dmDisplayFrequency > 1
            {
                frequency / i64::from(mode.dmDisplayFrequency)
            } else {
                0
            };
            self.refiner = Refiner::default();
        }
        let mut counter = 0;
        // SAFETY: `counter` is a local i64 that outlives the call.
        if unsafe { QueryPerformanceCounter(&mut counter) }.is_err() {
            return captured;
        }
        let now = Instant::now();
        let age = now.saturating_duration_since(captured);
        if age > Duration::from_secs(2) {
            return captured;
        }
        let composition = counter - (age.as_secs_f64() * frequency as f64) as i64;
        if composition >= self.next_poll {
            self.next_poll = composition + frequency / 4;
            // SAFETY: The call only writes the owner's id into `self.pid`; a null window yields 0.
            unsafe {
                GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut self.pid));
            }
        }
        let source = self.tracker.as_ref().unwrap().presents(
            self.pid,
            composition,
            self.chain,
            &mut self.presents,
        );
        if source != self.chain {
            self.chain = source;
            self.refiner.reset_source();
        }
        let refined = self.refiner.refine(
            composition,
            self.grid,
            &self.presents,
            frequency / 90000 + 1,
            frequency / 10,
        );
        let delta = Duration::from_secs_f64(
            (refined - composition).unsigned_abs() as f64 / frequency as f64,
        );
        if refined >= composition {
            captured.checked_add(delta).unwrap_or(captured)
        } else {
            captured.checked_sub(delta).unwrap_or(captured)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dxgi_event_abi_rejects_test_truncated_and_unrelated_presents() {
        let events = Events {
            values: Mutex::new(VecDeque::new()),
            retention: 1000,
        };
        for pointer in [4, 8] {
            let mut data = vec![0; pointer + 8];
            data[..4].copy_from_slice(&0x1234u32.to_le_bytes());
            let mut record = EVENT_RECORD {
                UserContext: (&events as *const Events).cast_mut().cast(),
                UserData: data.as_mut_ptr().cast(),
                UserDataLength: data.len() as u16,
                ..Default::default()
            };
            record.EventHeader.ProviderId = DXGI;
            record.EventHeader.EventDescriptor.Id = 42;
            record.EventHeader.ProcessId = 17;
            record.EventHeader.TimeStamp = 2000;
            record.EventHeader.Flags = if pointer == 4 { 0x20 } else { 0 };
            // SAFETY: `record` points at the live `events` and `data`, with UserDataLength within
            // `data`.
            unsafe {
                on_event(&mut record);
            }
            let saved = events.values.lock().unwrap().back().copied().unwrap();
            assert_eq!((saved.qpc, saved.pid, saved.chain), (2000, 17, 0x1234));
            let length = events.values.lock().unwrap().len();
            data[pointer] = 1; // DXGI_PRESENT_TEST
            // SAFETY: `record` points at the live `events` and `data`, with UserDataLength within
            // `data`.
            unsafe {
                on_event(&mut record);
            }
            data[pointer] = 0;
            record.UserDataLength = pointer as u16;
            // SAFETY: `record` points at the live `events` and `data`, with UserDataLength within
            // `data`.
            unsafe {
                on_event(&mut record);
            }
            record.UserDataLength = data.len() as u16;
            record.EventHeader.EventDescriptor.Id = 99;
            // SAFETY: `record` points at the live `events` and `data`, with UserDataLength within
            // `data`.
            unsafe {
                on_event(&mut record);
            }
            assert_eq!(events.values.lock().unwrap().len(), length);
        }
    }
}
