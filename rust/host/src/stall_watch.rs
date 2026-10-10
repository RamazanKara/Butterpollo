//! Logs where a stream thread is when it stops making progress
//! (`butterpollo_core::stall_watch`), with how long a trivial query on the
//! stream's GPU has been outstanding. A stalled GPU holds both; a thread stuck
//! on the host's own work (a lock, a fence, the network) only the thread.
//! Observation only: nothing in the stream changes.
use butterpollo_core::stall_watch::{Event, Watch};
use std::{
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// Longer than any normal wait in a stream thread (a 50 ms repeat, a 20 fps
/// static picture), short of a freeze anyone would report.
const LIMIT: Duration = Duration::from_millis(500);
const TICK: Duration = Duration::from_millis(50);
const PROBE_EVERY: Duration = Duration::from_millis(100);
/// A probe query slower than this is logged even without a thread stall.
const PROBE_LATE: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Loop,
    Picture,
    EncoderCreate,
    Encode,
    Output,
    Send,
    Capture,
    Reopen,
}
impl Phase {
    fn name(phase: u8) -> &'static str {
        [
            "stream loop",
            "waiting for a picture",
            "creating the encoder",
            "encoding",
            "collecting encoder output",
            "sending",
            "capturing",
            "reopening capture",
        ]
        .get(usize::from(phase))
        .copied()
        .unwrap_or("unknown")
    }
}

fn now() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_nanos() as u64 + 1
}

pub struct Progress {
    thread: &'static str,
    stream: String,
    phase: AtomicU8,
    at: AtomicU64,
    gpu: Option<Arc<GpuProbe>>,
}
impl Progress {
    pub fn mark(&self, phase: Phase) {
        self.phase.store(phase as u8, Ordering::Relaxed);
        self.at.store(now(), Ordering::Release);
    }
}

type Watched = Mutex<Vec<(Weak<Progress>, Watch)>>;

/// Watched until the returned handle is dropped.
pub fn register(thread: &'static str, stream: &str, gpu: Option<Arc<GpuProbe>>) -> Arc<Progress> {
    static WATCHED: OnceLock<Arc<Watched>> = OnceLock::new();
    let progress = Arc::new(Progress {
        thread,
        stream: stream.to_owned(),
        phase: AtomicU8::new(Phase::Loop as u8),
        at: AtomicU64::new(now()),
        gpu,
    });
    let watched = WATCHED.get_or_init(|| {
        let watched = Arc::new(Watched::default());
        let watcher = watched.clone();
        if let Err(error) = thread::Builder::new()
            .name("stall watch".into())
            .spawn(move || watch(&watcher))
        {
            tracing::warn!(%error, "stream stalls are not reported");
        }
        watched
    });
    watched
        .lock()
        .unwrap()
        .push((Arc::downgrade(&progress), Watch::default()));
    progress
}

fn watch(watched: &Watched) {
    loop {
        thread::sleep(TICK);
        let mut watched = watched.lock().unwrap();
        watched.retain(|(progress, _)| progress.strong_count() > 0);
        for (progress, watch) in watched.iter_mut() {
            let Some(progress) = progress.upgrade() else {
                continue;
            };
            let at = progress.at.load(Ordering::Acquire);
            let phase = progress.phase.load(Ordering::Relaxed);
            let event = watch.check(at, phase, false, now(), LIMIT.as_nanos() as u64);
            let (gpu_query_outstanding_ms, gpu_query_last_ms) = progress
                .gpu
                .as_ref()
                .map_or((None, None), |gpu| gpu.state());
            match event {
                Some(Event::Stalled { phase, ms }) => tracing::warn!(
                    thread = progress.thread,
                    stream_id = %progress.stream,
                    phase = Phase::name(phase),
                    stalled_ms = ms,
                    gpu_query_outstanding_ms,
                    gpu_query_last_ms,
                    "stream thread stopped making progress"
                ),
                Some(Event::Resumed { phase, ms }) => tracing::warn!(
                    thread = progress.thread,
                    stream_id = %progress.stream,
                    phase = Phase::name(phase),
                    stalled_ms = ms,
                    gpu_query_outstanding_ms,
                    gpu_query_last_ms,
                    "stream thread resumed after a stall"
                ),
                None => {}
            }
        }
    }
}

/// Times an event query on the stream's GPU every 100 ms, on a thread and
/// device of its own, until the last handle is dropped.
pub struct GpuProbe {
    /// When the outstanding query was issued (`now()`); 0 when none is.
    issued: AtomicU64,
    last_us: AtomicU64,
}
impl GpuProbe {
    pub fn start(display: &str, stream: &str) -> Arc<Self> {
        let probe = Arc::new(Self {
            issued: AtomicU64::new(0),
            last_us: AtomicU64::new(0),
        });
        let weak = Arc::downgrade(&probe);
        let (display, stream) = (display.to_owned(), stream.to_owned());
        let started = thread::Builder::new().name("gpu probe".into()).spawn(move || {
            let query = match butterpollo_windows::gpu_probe::Probe::new(&display) {
                Ok(query) => query,
                Err(error) => {
                    let output = &display;
                    tracing::info!(error = %format!("{error:#}"), display = %output, "GPU stall probe unavailable");
                    return;
                }
            };
            let output = &display;
            tracing::info!(display = %output, stream_id = %stream, "GPU stall probe opened");
            loop {
                thread::sleep(PROBE_EVERY);
                let Some(probe) = weak.upgrade() else { return };
                let issued = now();
                probe.issued.store(issued, Ordering::Release);
                // The probe holds no handle while it waits: the stream can end.
                drop(probe);
                query.issue();
                let mut reported = false;
                let done = loop {
                    match query.done() {
                        Ok(true) => break true,
                        Ok(false) => {}
                        Err(error) => {
                            tracing::info!(error = %format!("{error:#}"), %stream, "GPU stall probe stopped");
                            break false;
                        }
                    }
                    let waited = Duration::from_nanos(now() - issued);
                    if !reported && waited >= LIMIT {
                        reported = true;
                        tracing::warn!(stream_id = %stream, waited_ms = waited.as_millis(), "the GPU has not finished a trivial query");
                    }
                    if weak.strong_count() == 0 {
                        return;
                    }
                    thread::sleep(Duration::from_millis(1));
                };
                let took = Duration::from_nanos(now() - issued);
                let Some(probe) = weak.upgrade() else { return };
                probe.issued.store(0, Ordering::Release);
                probe.last_us.store(took.as_micros() as u64, Ordering::Relaxed);
                if !done {
                    return;
                }
                if took >= PROBE_LATE {
                    tracing::warn!(stream_id = %stream, gpu_query_ms = took.as_millis(), "a trivial GPU query finished late");
                }
            }
        });
        if let Err(error) = started {
            tracing::warn!(%error, "GPU stall probe not started");
        }
        probe
    }
    /// Milliseconds the outstanding query has waited, and the last one took.
    fn state(&self) -> (Option<u64>, Option<u64>) {
        let issued = self.issued.load(Ordering::Acquire);
        let last = self.last_us.load(Ordering::Relaxed);
        (
            (issued != 0).then(|| now().saturating_sub(issued) / 1_000_000),
            (last != 0).then_some(last / 1000),
        )
    }
}
