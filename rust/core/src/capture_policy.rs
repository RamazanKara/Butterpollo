//! WGC publication policy in nanoseconds, independent of Windows/GPU APIs.
#[derive(Default)]
pub struct Intervals {
    last: Option<i64>,
    samples: [i64; 16],
    count: usize,
    next: usize,
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
