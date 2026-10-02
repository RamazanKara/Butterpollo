//! WGC publication policy in nanoseconds, independent of Windows/GPU APIs.
#[derive(Default)]
pub struct Intervals {
    last: Option<i64>,
    samples: [i64; 16],
    count: usize,
    next: usize,
}

/// Predict a nearby publication only after a stable source cadence is known.
/// A slow, irregular, restarted or already fresh source keeps the normal slot.
#[derive(Default)]
pub struct Freshness {
    last: Option<u64>,
    intervals: [u64; 16],
    delays: [u64; 16],
    count: usize,
    next: usize,
}
impl Freshness {
    fn stable_period(&self) -> Option<u64> {
        if self.count < 8 {
            return None;
        }
        let mut intervals = self.intervals;
        intervals[..self.count].sort_unstable();
        let source = intervals[self.count / 2];
        (intervals[0] >= source.saturating_mul(7) / 8
            && intervals[self.count - 1] <= source.saturating_mul(9) / 8)
            .then_some(source)
    }
    /// Use a short polling window around one predicted publication. Once that
    /// window passes, a source that becomes static returns to the normal wait.
    pub fn poll_wait(&self, age: u64, normal: u64) -> u64 {
        let Some(source) = self.stable_period().filter(|source| *source >= 3_000_000) else {
            return normal;
        };
        let mut delays = self.delays;
        delays[..self.count].sort_unstable();
        let expected = source.saturating_add(delays[(self.count - 1) / 10]);
        let early = expected.saturating_sub(1_000_000);
        let late = expected.saturating_add(500_000);
        if age < early {
            normal.min(early - age)
        } else if age <= late {
            normal.min(100_000)
        } else {
            normal
        }
    }
    pub fn observe(&mut self, captured: u64, delay: u64) {
        if let Some(previous) = self.last {
            let interval = captured.saturating_sub(previous);
            if captured > previous && interval <= 500_000_000 {
                self.intervals[self.next] = interval;
                self.delays[self.next] = delay;
                self.next = (self.next + 1) % self.intervals.len();
                self.count = (self.count + 1).min(self.intervals.len());
            } else {
                self.count = 0;
                self.next = 0;
            }
        }
        self.last = Some(captured);
    }
    /// Nanoseconds to an expected publication, bounded to half the stream
    /// period and 4 ms. No prediction is returned beyond that latency budget.
    pub fn wait(&self, age: u64, period: u64) -> Option<u64> {
        if self.count < 8 || period == 0 || age <= period / 4 {
            return None;
        }
        let source = self.stable_period()?;
        if source > period.saturating_mul(11) / 10 {
            return None;
        }
        let mut delays = self.delays;
        delays[..self.count].sort_unstable();
        let expected = source
            .saturating_add(delays[(self.count - 1) * 90 / 100])
            .saturating_add(250_000);
        let wait = expected.saturating_sub(age);
        (wait > 0 && wait <= (period / 2).min(4_000_000)).then_some(wait)
    }
}
impl Intervals {
    pub fn observe(&mut self, now: i64) -> i64 {
        if let Some(previous) = self.last {
            let period = now - previous;
            if (1..=500_000_000).contains(&period) {
                self.samples[self.next] = period;
                self.next = (self.next + 1) % self.samples.len();
                self.count = (self.count + 1).min(self.samples.len());
            }
        }
        self.last = Some(now);
        if self.count < 4 {
            return 0;
        }
        let mut sorted = self.samples;
        sorted[..self.count].sort_unstable();
        sorted[self.count / 2]
    }
}
/// Defer a copy only when a newer composition can precede the same encode slot.
/// The deadline preserves a lone update if the source becomes static.
pub fn publication_deadline(
    period: i64,
    now: i64,
    composition: i64,
    last_publish: i64,
) -> Option<i64> {
    if period <= 0 || composition <= 0 || composition.saturating_mul(5) > period.saturating_mul(3) {
        return None;
    }
    let next = if now < 0 {
        0
    } else {
        (now / period + 1).saturating_mul(period)
    };
    let previous = next - period;
    if now - previous < 6_000_000 && last_publish <= previous - period {
        return None;
    }
    if now.saturating_add(composition).saturating_add(200_000) > next {
        return None;
    }
    let deadline = next - 250_000;
    (deadline > now).then_some(deadline)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn learned(interval: u64) -> Freshness {
        let mut cadence = Freshness::default();
        for frame in 0..=16 {
            cadence.observe(frame * interval, 500_000);
        }
        cadence
    }
    #[test]
    fn nearby_faster_and_equal_rate_publications_fit_the_latency_budget() {
        let fast = learned(4_166_667);
        assert_eq!(fast.wait(3_000_000, 8_333_333), Some(1_916_667));
        assert_eq!(fast.wait(1_000_000, 8_333_333), None);
        let equal = learned(8_333_333);
        assert_eq!(equal.wait(7_000_000, 8_333_333), Some(2_083_333));
        assert_eq!(equal.wait(3_000_000, 8_333_333), None);
        assert_eq!(learned(16_666_667).wait(7_000_000, 8_333_333), None);
        assert_eq!(fast.wait(30_000_000, 8_333_333), None);
    }
    #[test]
    fn unknown_irregular_and_restarted_sources_do_not_add_a_wait() {
        let mut cadence = learned(4_166_667);
        cadence.observe(16 * 4_166_667 + 20_000_000, 500_000);
        assert_eq!(cadence.wait(3_000_000, 8_333_333), None);
        cadence.observe(1_000_000_000, 500_000);
        assert_eq!(cadence.wait(3_000_000, 8_333_333), None);
        cadence.observe(1, 500_000);
        assert_eq!(cadence.wait(3_000_000, 8_333_333), None);
        assert_eq!(Freshness::default().wait(7_000_000, 8_333_333), None);
        assert_eq!(learned(4_166_667).wait(3_000_000, 0), None);
    }
    #[test]
    fn predictive_polling_is_bounded_and_static_or_irregular_sources_use_normal_waits() {
        let mut cadence = learned(8_333_333);
        assert_eq!(cadence.poll_wait(2_000_000, 1_000_000), 1_000_000);
        assert_eq!(cadence.poll_wait(7_500_000, 1_000_000), 333_333);
        assert_eq!(cadence.poll_wait(8_000_000, 1_000_000), 100_000);
        assert_eq!(cadence.poll_wait(10_000_000, 1_000_000), 1_000_000);
        assert_eq!(cadence.poll_wait(1_000_000_000, 1_000_000), 1_000_000);
        cadence.observe(16 * 8_333_333 + 20_000_000, 500_000);
        assert_eq!(cadence.poll_wait(8_000_000, 1_000_000), 1_000_000);
        assert_eq!(
            Freshness::default().poll_wait(8_000_000, 1_000_000),
            1_000_000
        );
        assert_eq!(
            learned(1_000_000).poll_wait(1_000_000, 1_000_000),
            1_000_000
        );
    }
    #[test]
    fn publication_preserves_static_updates_and_a_waiting_encode_slot() {
        assert_eq!(
            publication_deadline(20_000_000, 3_000_000, 8_333_333, 0),
            Some(19_750_000)
        );
        assert_eq!(
            publication_deadline(20_000_000, 19_000_000, 8_333_333, 0),
            None
        );
        assert_eq!(
            publication_deadline(20_000_000, 21_000_000, 8_333_333, 0),
            None
        );
        assert_eq!(
            publication_deadline(20_000_000, 3_000_000, 12_001_000, 0),
            None
        );
        assert_eq!(publication_deadline(0, 3, 1, 0), None);
        let mut history = Intervals::default();
        for time in [0, 8_000_000, 16_000_000, 24_000_000] {
            assert_eq!(history.observe(time), 0);
        }
        assert_eq!(history.observe(32_000_000), 8_000_000);
        assert_eq!(history.observe(432_000_000), 8_000_000);
    }
}
