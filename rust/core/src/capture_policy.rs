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
    median: Option<u64>,
    polling_window: Option<(u64, u64)>,
}
impl Freshness {
    /// The median interval between recent source frames, once a few are known.
    /// The capture worker observes every frame, so this is the source cadence
    /// even when a consumer only looks at some of them.
    pub fn median_interval(&self) -> Option<u64> {
        self.median
    }
    /// Use a short polling window around one predicted publication. Once that
    /// window passes, a source that becomes static returns to the normal wait.
    pub fn poll_wait(&self, age: u64, normal: u64) -> u64 {
        let Some((early, late)) = self.polling_window else {
            return normal;
        };
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
        // Polling reads the same history many times between frames. Recompute
        // its statistics only when that history changes, outside the wait loop.
        self.median = None;
        self.polling_window = None;
        if self.count >= 4 {
            let mut intervals = self.intervals;
            intervals[..self.count].sort_unstable();
            let source = intervals[self.count / 2];
            self.median = Some(source);
            if self.count >= 8
                && source >= 3_000_000
                && intervals[0] >= source.saturating_mul(7) / 8
                && intervals[self.count - 1] <= source.saturating_mul(9) / 8
            {
                let mut delays = self.delays;
                delays[..self.count].sort_unstable();
                let expected = source.saturating_add(delays[(self.count - 1) / 10]);
                self.polling_window = Some((
                    expected.saturating_sub(1_000_000),
                    expected.saturating_add(500_000),
                ));
            }
        }
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
    fn cached_statistics_match_the_original_policy_across_cadence_changes_and_resets() {
        fn reference(cadence: &Freshness, age: u64, normal: u64) -> u64 {
            if cadence.count < 8 {
                return normal;
            }
            let mut intervals = cadence.intervals;
            intervals[..cadence.count].sort_unstable();
            let source = intervals[cadence.count / 2];
            if source < 3_000_000
                || intervals[0] < source.saturating_mul(7) / 8
                || intervals[cadence.count - 1] > source.saturating_mul(9) / 8
            {
                return normal;
            }
            let mut delays = cadence.delays;
            delays[..cadence.count].sort_unstable();
            let expected = source.saturating_add(delays[(cadence.count - 1) / 10]);
            let early = expected.saturating_sub(1_000_000);
            if age < early {
                normal.min(early - age)
            } else if age <= expected.saturating_add(500_000) {
                normal.min(100_000)
            } else {
                normal
            }
        }
        let mut cadence = Freshness::default();
        let mut captured = 1_000_000_000u64;
        for frame in 0..2000u64 {
            let period = [16_666_667, 8_333_333, 2_000_000, 20_000_000][(frame / 40 % 4) as usize];
            captured = match frame % 97 {
                0 => captured.saturating_sub(50_000_000),
                1 => captured,
                2 => captured + 600_000_000,
                _ => captured + period + (frame % 5) * 10_000,
            };
            cadence.observe(captured, (frame % 17) * 100_000);
            let mut intervals = cadence.intervals;
            intervals[..cadence.count].sort_unstable();
            assert_eq!(
                cadence.median_interval(),
                (cadence.count >= 4).then(|| intervals[cadence.count / 2])
            );
            for age in [0, period - 1_000_000, period, period + 2_000_000, u64::MAX] {
                for normal in [50_000, 1_000_000, 10_000_000] {
                    assert_eq!(
                        cadence.poll_wait(age, normal),
                        reference(&cadence, age, normal)
                    );
                }
            }
        }
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
