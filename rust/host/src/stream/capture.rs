//! Latest-frame ownership and notifications shared by capture consumers.
use anyhow::{Result, bail};
use butterpollo_windows::timing::{Signal, Timer};
use std::{
    sync::{Arc, Mutex, Weak},
    time::Instant,
};

struct State<T> {
    image: Option<Arc<T>>,
    error: Option<String>,
}
impl<T> State<T> {
    fn check(&self) -> Result<()> {
        if let Some(error) = &self.error {
            bail!("capture stopped: {error}");
        }
        Ok(())
    }
}
pub(super) struct Latest<T> {
    state: Mutex<State<T>>,
    changed: Mutex<Vec<Weak<Signal>>>,
}
impl<T> Latest<T> {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                image: None,
                error: None,
            }),
            changed: Mutex::new(Vec::new()),
        }
    }
    pub(super) fn subscribe(&self) -> Result<Arc<Signal>> {
        let signal = Arc::new(Signal::new()?);
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
    pub(super) fn publish(&self, image: Option<Arc<T>>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.check()?;
        state.image = image;
        // Reset, predicate checks and both frame/error notifications share this
        // lock. A notification cannot be lost immediately before a wait.
        self.notify()
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
        predicate: impl FnOnce(Option<&Arc<T>>) -> bool,
    ) -> Result<()> {
        let state = self.state.lock().unwrap();
        state.check()?;
        if predicate(state.image.as_ref()) {
            wake.reset()?;
            drop(state);
            timer.until_or_signal(deadline, wake)?;
        }
        Ok(())
    }
    pub(super) fn wait_for_frame(
        &self,
        timer: &Timer,
        wake: &Signal,
        deadline: Instant,
    ) -> Result<Option<Arc<T>>> {
        self.wait_if(timer, wake, deadline, |image| image.is_none())?;
        self.current()
    }
    pub(super) fn wait_if_current(
        &self,
        timer: &Timer,
        wake: &Signal,
        image: &Arc<T>,
        deadline: Instant,
    ) -> Result<()> {
        self.wait_if(timer, wake, deadline, |current| {
            current.is_some_and(|current| Arc::ptr_eq(current, image))
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
        latest.publish(Some(old.clone()))?;
        latest.wait_if_current(
            &timer,
            &first,
            &old,
            Instant::now() + Duration::from_millis(2),
        )?;
        assert!(timer.until_or_signal(Instant::now() + Duration::from_secs(1), &second)?);
        let new = Arc::new(2u8);
        latest.publish(Some(new.clone()))?;
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
        latest.publish(None)?;
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
        assert!(latest.publish(Some(Arc::new(3))).is_err());
        let late = latest.subscribe()?;
        assert!(
            latest
                .wait_for_frame(&timer, &late, Instant::now())
                .is_err()
        );
        Ok(())
    }
}
