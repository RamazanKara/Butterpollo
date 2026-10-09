//! Latest-frame ownership and notifications shared by capture consumers.
use anyhow::{Result, bail};
use butterpollo_windows::device_loss::DeviceLost;
use butterpollo_windows::timing::{Signal, Timer};
use std::{
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

/// Diagnostic identities outlive replacement of the latest frame without
/// keeping the frame or its GPU resources alive. Only TRACE publications add
/// records; expired records are pruned by the next traced publication.
struct PublicationTrace<T> {
    next_id: u64,
    live: Vec<(Weak<T>, u64)>,
}
impl<T> PublicationTrace<T> {
    fn new() -> Self {
        Self {
            next_id: 0,
            live: Vec::new(),
        }
    }
    fn observe(&mut self, image: &Arc<T>) -> u64 {
        self.live.retain(|(image, _)| image.strong_count() > 0);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.live.push((Arc::downgrade(image), self.next_id));
        self.next_id
    }
    fn id(&self, image: &Arc<T>) -> Option<u64> {
        self.live
            .iter()
            .rev()
            .find(|(recorded, _)| recorded.as_ptr() == Arc::as_ptr(image))
            .map(|(_, id)| *id)
    }
}

struct State<T> {
    image: Option<Arc<T>>,
    error: Option<String>,
    captured: Option<Instant>,
    cadence: butterpollo_core::capture_policy::Freshness,
    generation: u64,
    trace: PublicationTrace<T>,
    recovery: super::device_recovery::DeviceRecovery,
}
impl<T> State<T> {
    fn check(&self) -> Result<()> {
        if let Some(error) = &self.error {
            bail!("capture stopped: {error}");
        }
        if self.recovery.active() {
            self.recovery.check(Instant::now())?;
        }
        Ok(())
    }
}
/// Each stream acknowledges a reset only after releasing its encoder, images
/// and filters. Dropping the subscription removes a departing stream's lease.
pub(super) struct Consumer {
    signal: Signal,
    released: AtomicU64,
    session: Weak<crate::state::Session>,
    recovery_pending: AtomicBool,
}
impl Consumer {
    pub(super) fn wake_on_recovery<P, A>(
        self: &Arc<Self>,
        session: &butterpollo_core::session::Session<P, A>,
    ) {
        let wake = Arc::downgrade(self);
        *session.recovery_wake.lock().unwrap() = Some(Box::new(move || {
            if let Some(wake) = wake.upgrade() {
                wake.recovery_pending.store(true, Ordering::Release);
                let _ = wake.set();
            }
        }));
    }
    fn prepare_wait(&self) -> Result<bool> {
        self.signal.reset()?;
        // A recovery request before the reset must still bypass the wait.
        Ok(!self.recovery_pending.swap(false, Ordering::AcqRel))
    }
}
impl std::ops::Deref for Consumer {
    type Target = Signal;
    fn deref(&self) -> &Signal {
        &self.signal
    }
}
pub(super) struct Latest<T> {
    state: Mutex<State<T>>,
    changed: Mutex<Vec<Weak<Consumer>>>,
    origin: Instant,
    trace_source_id: u64,
    recovering: AtomicBool,
}
impl<T> Latest<T> {
    pub(super) fn new() -> Self {
        static NEXT_SOURCE_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            state: Mutex::new(State {
                image: None,
                error: None,
                captured: None,
                cadence: Default::default(),
                generation: 0,
                trace: PublicationTrace::new(),
                recovery: Default::default(),
            }),
            changed: Mutex::new(Vec::new()),
            origin: Instant::now(),
            trace_source_id: NEXT_SOURCE_ID.fetch_add(1, Ordering::Relaxed),
            recovering: AtomicBool::new(false),
        }
    }
    pub(super) fn subscribe(&self, session: Weak<crate::state::Session>) -> Result<Arc<Consumer>> {
        let state = self.state.lock().unwrap();
        let signal = Arc::new(Consumer {
            signal: Signal::new()?,
            // A new subscriber owns nothing from an earlier generation.
            released: AtomicU64::new(state.generation),
            session,
            recovery_pending: AtomicBool::new(false),
        });
        let mut waiters = self.changed.lock().unwrap();
        waiters.retain(|waiter| waiter.strong_count() > 0);
        waiters.push(Arc::downgrade(&signal));
        Ok(signal)
    }
    fn notify(&self) -> Result<()> {
        let mut waiters = self.changed.lock().unwrap();
        waiters.retain(|waiter| waiter.strong_count() > 0);
        for waiter in waiters.iter().filter_map(Weak::upgrade) {
            waiter.set()?;
        }
        Ok(())
    }
    pub(super) fn begin_recovery(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.check()?;
        state.generation = state.generation.wrapping_add(1);
        state.image = None;
        state.captured = None;
        state.cadence = Default::default();
        self.notify()
    }
    pub(super) fn device_lost(
        self: &Arc<Self>,
        loss: DeviceLost,
        warnings: &Arc<butterpollo_core::session::Warnings>,
        now: Instant,
    ) -> Result<()>
    where
        T: Send + Sync + 'static,
    {
        let mut state = self.state.lock().unwrap();
        state.check()?;
        if state.recovery.lost(loss, now) {
            warnings.set("gpu_recovery", format!("{loss}. Rebuilding capture and encoder; the picture may freeze for up to 30 seconds. If the adapter remains unavailable (Code 31), reboot Windows before reconnecting."));
            // Vendor teardown can block inside a DLL. End the clients independently
            // of that worker; Windows cannot safely cancel an in-process driver call.
            let latest = Arc::downgrade(self);
            let warnings = warnings.clone();
            let started = state.recovery.started();
            std::thread::Builder::new()
                .name("gpu-recovery".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        let Some(latest) = latest.upgrade() else {
                            break;
                        };
                        let state = latest.state.lock().unwrap();
                        if state.recovery.started() != started || state.error.is_some() {
                            break;
                        }
                        let result = state.recovery.check(Instant::now());
                        drop(state);
                        if let Err(error) = result {
                            warnings.set("gpu_recovery", format!("{error:#}"));
                            let _ = latest.fail(format!("{error:#}"));
                            break;
                        }
                    }
                })?;
        }
        self.recovering.store(true, Ordering::Release);
        let image = state.image.take();
        let result = self.notify();
        drop(state);
        drop(image);
        result
    }
    pub(super) fn take_device_restart(&self) -> Result<bool> {
        let mut state = self.state.lock().unwrap();
        state.check()?;
        Ok(std::mem::take(&mut state.recovery.restart))
    }
    pub(super) fn device_recovered(&self, warnings: &butterpollo_core::session::Warnings) {
        if !self.recovering.load(Ordering::Acquire) {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if state.error.is_none()
            && state.recovery.check(Instant::now()).is_ok()
            && state.recovery.recovered()
        {
            self.recovering.store(false, Ordering::Release);
            warnings.clear("gpu_recovery");
            warnings.event("gpu_reset", "The graphics device was reset; capture and encoding have resumed with a keyframe. Check the graphics driver if this repeats.", butterpollo_core::session::EVENT_PERIOD);
            tracing::info!("GPU recovery completed; encoded output resumed");
        }
    }
    /// Call only after releasing every resource belonging to this capture.
    pub(super) fn release_generation(&self, consumer: &Consumer) {
        let state = self.state.lock().unwrap();
        consumer.released.store(state.generation, Ordering::Release);
    }
    pub(super) fn consumers_released(&self) -> bool {
        let state = self.state.lock().unwrap();
        self.changed
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .all(|consumer| consumer.released.load(Ordering::Acquire) == state.generation)
    }
    pub(super) fn publish_captured(&self, image: Arc<T>, captured: Instant) -> Result<()> {
        let now = Instant::now();
        let nanos =
            |duration: std::time::Duration| duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        let mut state = self.state.lock().unwrap();
        state.check()?;
        if state.recovery.restart {
            return Ok(());
        }
        state.recovery.captured = true;
        state.cadence.observe(
            nanos(captured.saturating_duration_since(self.origin)),
            nanos(now.saturating_duration_since(captured)),
        );
        if tracing::enabled!(target: "pacing", tracing::Level::TRACE) {
            let capture_id = state.trace.observe(&image);
            tracing::trace!(
                target: "pacing",
                capture_id,
                generation = state.generation,
                captured_ns = nanos(captured.saturating_duration_since(self.origin)),
                published_ns = nanos(now.saturating_duration_since(self.origin)),
                age_ns = nanos(now.saturating_duration_since(captured)),
                source_id = self.trace_source_id,
                "publish"
            );
        }
        state.captured = Some(captured);
        state.image = Some(image);
        self.notify()
    }
    /// Match a claimed frame even if capture already published its replacement.
    /// Call only while emitting pacing traces; untraced frames have no identity.
    pub(super) fn trace_publication_id(&self, image: &Arc<T>) -> Option<u64> {
        self.state.lock().unwrap().trace.id(image)
    }
    pub(super) fn trace_source_id(&self) -> u64 {
        self.trace_source_id
    }
    /// The source's recent frame interval, as observed by the capture worker.
    pub(super) fn source_interval(&self) -> Option<std::time::Duration> {
        self.state
            .lock()
            .unwrap()
            .cadence
            .median_interval()
            .map(std::time::Duration::from_nanos)
    }
    pub(super) fn poll_interval(&self, normal: std::time::Duration) -> std::time::Duration {
        let state = self.state.lock().unwrap();
        let Some(captured) = state.captured else {
            return normal;
        };
        let nanos =
            |duration: std::time::Duration| duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        std::time::Duration::from_nanos(
            state
                .cadence
                .poll_wait(nanos(captured.elapsed()), nanos(normal)),
        )
    }
    pub(super) fn fail(&self, error: String) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        for consumer in self
            .changed
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
        {
            if let Some(session) = consumer.session.upgrade() {
                session
                    .launch
                    .warnings
                    .set("stream_failure", format!("Stream ended: {error}"));
                session.fail();
            }
        }
        let image = state.image.take();
        state.error = Some(error);
        let result = self.notify();
        drop(state);
        drop(image);
        result
    }
    pub(super) fn current(&self) -> Result<Option<Arc<T>>> {
        let state = self.state.lock().unwrap();
        state.check()?;
        Ok(state.image.clone())
    }
    pub(super) fn check(&self) -> Result<()> {
        self.state.lock().unwrap().check()
    }
    fn wait_if(
        &self,
        timer: &Timer,
        wake: &Consumer,
        deadline: Instant,
        precise: bool,
        predicate: impl FnOnce(&State<T>) -> bool,
    ) -> Result<()> {
        let state = self.state.lock().unwrap();
        state.check()?;
        if predicate(&state) && wake.prepare_wait()? {
            drop(state);
            if precise {
                timer.until_or_signal_precise(deadline, wake)?;
            } else {
                timer.until_or_signal(deadline, wake)?;
            }
        }
        Ok(())
    }
    pub(super) fn wait_for_frame(
        &self,
        timer: &Timer,
        wake: &Consumer,
        deadline: Instant,
    ) -> Result<Option<Arc<T>>> {
        self.wait_if(timer, wake, deadline, false, |state| {
            // A reset must release the encoder immediately, not wait another
            // frame period while the capture worker waits for its release.
            state.image.is_none() && wake.released.load(Ordering::Acquire) == state.generation
        })?;
        self.current()
    }
    pub(super) fn wait_if_current(
        &self,
        timer: &Timer,
        wake: &Consumer,
        image: &Arc<T>,
        deadline: Instant,
    ) -> Result<()> {
        self.wait_if(timer, wake, deadline, false, |state| {
            state
                .image
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, image))
        })
    }
    /// wait_if_current to a frame's claim deadline, met to the tenth of a
    /// millisecond: a late wake delays that frame.
    pub(super) fn wait_if_current_precise(
        &self,
        timer: &Timer,
        wake: &Consumer,
        image: &Arc<T>,
        deadline: Instant,
    ) -> Result<()> {
        self.wait_if(timer, wake, deadline, true, |state| {
            state
                .image
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, image))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn pending_idr_is_served_within_the_short_poll_instead_of_a_stream_period() -> Result<()> {
        use butterpollo_core::{
            rtsp::Negotiated,
            session::{Launch, Role, Session},
            state::Client,
        };
        let session = Session::<(), ()>::new(
            Launch {
                id: "recovery".into(),
                client: Client {
                    name: "fixture".into(),
                    cert: String::new(),
                    uuid: "client".into(),
                    perm: u32::MAX,
                    enabled: true,
                    extra: Default::default(),
                },
                peer: "127.0.0.1".parse().unwrap(),
                app_id: 1,
                key: [0; 16],
                key_id: 1,
                ping: "ping".into(),
                connect_data: 1,
                role: Role::Stream,
                created: Instant::now(),
                rtsp_encrypted: true,
                rtsp_counter: Default::default(),
                rtsp_received: Default::default(),
                preparation: Default::default(),
                vrr_requested: false,
                host_audio: false,
                requested_rate: 0,
                options: Default::default(),
                audio_preparation: Default::default(),
                preparing: Default::default(),
                warnings: Default::default(),
            },
            Negotiated::default(),
        );
        session.idr.store(false, Ordering::Release);
        let latest = Latest::new();
        let image = Arc::new(1u8);
        latest.publish_captured(image.clone(), Instant::now())?;
        let wake = latest.subscribe(Weak::new())?;
        wake.wake_on_recovery(&session);
        assert!(wake.prepare_wait()?);

        session.request_idr();
        // Model the wait without scheduler jitter: resetting the capture event
        // must not postpone a pending IDR until the static screen's next repeat.
        let now = Instant::now();
        let period = Duration::from_secs_f64(1. / 120.);
        let served = if wake.prepare_wait()? {
            now + period
        } else {
            now
        };
        assert!(served <= now + super::super::OUTPUT_POLL);
        assert!(session.idr.swap(false, Ordering::AcqRel));
        assert!(Arc::ptr_eq(&latest.current()?.unwrap(), &image));
        assert!(wake.prepare_wait()?);

        session.request_invalidation(4, 6);
        assert!(!wake.prepare_wait()?);
        assert_eq!(session.invalidation.lock().unwrap().take(), Some((4, 6)));
        assert!(wake.prepare_wait()?);

        // A request after the event reset must also interrupt the next wait.
        let timer = Timer::new()?;
        session.request_idr();
        assert!(timer.until_or_signal(Instant::now() + Duration::from_secs(1), &wake)?);
        Ok(())
    }
    #[test]
    fn publication_trace_matches_frames_still_owned_after_replacement() {
        let mut trace = PublicationTrace::new();
        let old = Arc::new(1u8);
        let old_id = trace.observe(&old);
        let current = Arc::new(2u8);
        let current_id = trace.observe(&current);
        assert!(current_id > old_id);
        assert_eq!(trace.id(&old), Some(old_id));
        assert_eq!(trace.id(&current), Some(current_id));
        assert_eq!(trace.id(&Arc::new(1u8)), None);
    }
    #[test]
    fn publication_trace_does_not_keep_capture_resources_alive() {
        let mut trace = PublicationTrace::new();
        let frame = Arc::new(1u8);
        let released = Arc::downgrade(&frame);
        let old_id = trace.observe(&frame);
        assert_eq!(Arc::strong_count(&frame), 1);
        drop(frame);
        assert!(released.upgrade().is_none());
        let replacement = Arc::new(2u8);
        let new_id = trace.observe(&replacement);
        assert!(new_id > old_id);
        assert_eq!(trace.live.len(), 1);
        assert_eq!(trace.id(&replacement), Some(new_id));
    }
    #[test]
    fn publications_before_wait_and_independent_consumers_keep_the_latest_frame() -> Result<()> {
        let latest = Latest::new();
        let timer = Timer::new()?;
        let first = latest.subscribe(Weak::new())?;
        let second = latest.subscribe(Weak::new())?;
        let old = Arc::new(1u8);
        latest.publish_captured(old.clone(), Instant::now())?;
        latest.wait_if_current(
            &timer,
            &first,
            &old,
            Instant::now() + Duration::from_millis(2),
        )?;
        assert!(timer.until_or_signal(Instant::now() + Duration::from_secs(1), &second)?);
        let new = Arc::new(2u8);
        latest.publish_captured(new.clone(), Instant::now())?;
        latest.wait_if_current(
            &timer,
            &first,
            &old,
            Instant::now() + Duration::from_secs(1),
        )?;
        assert!(Arc::ptr_eq(
            &latest
                .wait_for_frame(&timer, &first, Instant::now())?
                .unwrap(),
            &new
        ));
        latest.begin_recovery()?;
        assert!(
            latest
                .wait_for_frame(&timer, &first, Instant::now() + Duration::from_millis(2))?
                .is_none()
        );
        Ok(())
    }
    #[test]
    fn terminal_capture_failure_wakes_waiters_and_cannot_return_a_stale_frame() -> Result<()> {
        let latest = Arc::new(Latest::<u8>::new());
        let timer = Timer::new()?;
        let wake = latest.subscribe(Weak::new())?;
        let worker = latest.clone();
        let failed = std::thread::spawn(move || worker.fail("device lost".into()).unwrap());
        let error = latest
            .wait_for_frame(&timer, &wake, Instant::now() + Duration::from_secs(1))
            .unwrap_err();
        failed.join().unwrap();
        assert!(error.to_string().contains("device lost"));
        assert!(latest.current().is_err());
        assert!(
            latest
                .publish_captured(Arc::new(3), Instant::now())
                .is_err()
        );
        let late = latest.subscribe(Weak::new())?;
        assert!(
            latest
                .wait_for_frame(&timer, &late, Instant::now())
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn recovery_waits_for_all_owners_and_departing_consumers_release_their_lease() -> Result<()> {
        let latest = Latest::new();
        let first = latest.subscribe(Weak::new())?;
        let second = latest.subscribe(Weak::new())?;
        latest.publish_captured(Arc::new(1u8), Instant::now())?;
        let held = latest.current()?.unwrap();
        let old = Arc::downgrade(&held);
        latest.begin_recovery()?;
        assert!(latest.current()?.is_none());
        assert!(!latest.consumers_released());
        drop(held);
        latest.release_generation(&first);
        assert!(old.upgrade().is_none());
        assert!(!latest.consumers_released());
        // A stream joining during recovery has no old resources to release.
        let joining = latest.subscribe(Weak::new())?;
        drop(second);
        assert!(latest.consumers_released());
        latest.publish_captured(Arc::new(2), Instant::now())?;
        latest.begin_recovery()?;
        assert!(!latest.consumers_released());
        latest.release_generation(&first);
        assert!(!latest.consumers_released());
        latest.release_generation(&joining);
        assert!(latest.consumers_released());
        Ok(())
    }

    #[test]
    fn reset_wakes_each_waiter_and_skips_waiting_until_resources_are_released() -> Result<()> {
        let latest = Arc::new(Latest::<u8>::new());
        let consumer = latest.subscribe(Weak::new())?;
        let worker = latest.clone();
        let timer = Timer::new()?;
        let reset = std::thread::spawn(move || worker.begin_recovery().unwrap());
        let start = Instant::now();
        assert!(
            latest
                .wait_for_frame(&timer, &consumer, start + Duration::from_secs(2))?
                .is_none()
        );
        reset.join().unwrap();
        // An unacknowledged reset must also bypass waits started after notification.
        assert!(
            latest
                .wait_for_frame(&timer, &consumer, start + Duration::from_secs(2))?
                .is_none()
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(!latest.consumers_released());
        latest.release_generation(&consumer);
        assert!(latest.consumers_released());
        let frame = Arc::new(2);
        latest.publish_captured(frame.clone(), Instant::now())?;
        assert!(Arc::ptr_eq(
            &frame,
            &latest
                .wait_for_frame(&timer, &consumer, Instant::now())?
                .unwrap()
        ));
        Ok(())
    }
}
