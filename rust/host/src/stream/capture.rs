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

struct State<T> {
    image: Option<Arc<T>>,
    error: Option<String>,
    captured: Option<Instant>,
    cadence: butterpollo_core::capture_policy::Freshness,
    generation: u64,
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
}
impl<T> Latest<T> {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                image: None,
                error: None,
                captured: None,
                cadence: Default::default(),
                generation: 0,
            }),
            changed: Mutex::new(Vec::new()),
            origin: Instant::now(),
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
        state.captured = Some(captured);
        state.image = Some(image);
        self.notify()
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
        predicate: impl FnOnce(&State<T>) -> bool,
    ) -> Result<()> {
        let state = self.state.lock().unwrap();
        state.check()?;
        if predicate(&state) {
            wake.reset()?;
            drop(state);
            timer.until_or_signal(deadline, wake)?;
        }
        Ok(())
    }
    pub(super) fn wait_for_frame(
        &self,
        timer: &Timer,
        wake: &Consumer,
        deadline: Instant,
    ) -> Result<Option<Arc<T>>> {
        self.wait_if(timer, wake, deadline, |state| {
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
        self.wait_if(timer, wake, deadline, |state| {
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
