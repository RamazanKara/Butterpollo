//! Latest-frame ownership and notifications shared by capture consumers.
use anyhow::{Result, bail};
use butterpollo_windows::timing::{Signal, Timer};
use std::{
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
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
}
impl<T> State<T> {
    fn check(&self) -> Result<()> {
        if let Some(error) = &self.error {
            bail!("capture stopped: {error}");
        }
        Ok(())
    }
}
/// Each stream acknowledges a reset only after releasing its encoder, images
/// and filters. Dropping the subscription removes a departing stream's lease.
pub(super) struct Consumer {
    signal: Signal,
    released: AtomicU64,
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
            }),
            changed: Mutex::new(Vec::new()),
            origin: Instant::now(),
            trace_source_id: NEXT_SOURCE_ID.fetch_add(1, Ordering::Relaxed),
        }
    }
    pub(super) fn subscribe(&self) -> Result<Arc<Consumer>> {
        let state = self.state.lock().unwrap();
        let signal = Arc::new(Consumer {
            signal: Signal::new()?,
            // A new subscriber owns nothing from an earlier generation.
            released: AtomicU64::new(state.generation),
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
        state.image = None;
        state.error = Some(error);
        self.notify()
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
        wake: &Signal,
        deadline: Instant,
        precise: bool,
        predicate: impl FnOnce(&State<T>) -> bool,
    ) -> Result<()> {
        let state = self.state.lock().unwrap();
        state.check()?;
        if predicate(&state) {
            wake.reset()?;
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
        wake: &Signal,
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
        wake: &Signal,
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
        let first = latest.subscribe()?;
        let second = latest.subscribe()?;
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
        let wake = latest.subscribe()?;
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
        let late = latest.subscribe()?;
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
        let first = latest.subscribe()?;
        let second = latest.subscribe()?;
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
        let joining = latest.subscribe()?;
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
        let consumer = latest.subscribe()?;
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
