//! Vibepollo 2.0 present-cadence policy, using one QPC clock for all inputs.
//! A match can only move a frame within its display refresh interval.
#[derive(Clone, Debug)]
pub struct Refiner {
    samples: [i64; 64],
    count: usize,
    index: usize,
    consumed: Option<i64>,
    stamp: Option<i64>,
    composition: Option<i64>,
    violations: u8,
}
impl Default for Refiner {
    fn default() -> Self {
        Self {
            samples: [0; 64],
            count: 0,
            index: 0,
            consumed: None,
            stamp: None,
            composition: None,
            violations: 0,
        }
    }
}
impl Refiner {
    pub fn reset_source(&mut self) {
        self.consumed = None;
        self.count = 0;
        self.index = 0;
    }
    pub fn grid_disabled(&self) -> bool {
        self.violations >= 3
    }
    fn latency(&self) -> i64 {
        if self.count == 0 {
            return 0;
        }
        let mut values = self.samples;
        *values[..self.count].select_nth_unstable(self.count / 2).1
    }
    pub fn refine(
        &mut self,
        composition: i64,
        mut grid: i64,
        presents: &[i64],
        step: i64,
        stale: i64,
    ) -> i64 {
        if grid > 0
            && self
                .composition
                .is_some_and(|c| composition > c && composition - c < grid * 3 / 4)
        {
            self.violations = self.violations.saturating_add(1);
        }
        self.composition = Some(composition);
        if self.grid_disabled() {
            grid = 0;
        }
        let mut stamp = composition;
        if grid > 0 {
            stamp -= grid / 2;
            let excluded = self
                .consumed
                .unwrap_or(composition - stale - 1)
                .max(composition - stale - 1);
            let first = presents.partition_point(|p| *p <= excluded);
            let last = presents.partition_point(|p| *p <= composition);
            if first < last {
                let lead = self.latency() - grid / 2;
                let present = if self.count < 16 {
                    presents[first]
                } else {
                    presents[first..last]
                        .iter()
                        .rev()
                        .copied()
                        .find(|p| *p + lead <= composition)
                        .unwrap_or(presents[first])
                };
                self.consumed = Some(present);
                let sample = composition - present;
                if (0..=stale).contains(&sample) {
                    self.samples[self.index] = sample;
                    self.index = (self.index + 1) % 64;
                    self.count = (self.count + 1).min(64);
                }
                if self.count >= 16 {
                    stamp = (present + self.latency() - grid / 2)
                        .clamp(composition - grid + 1, composition);
                }
            }
        }
        if let Some(previous) = self.stamp {
            stamp = stamp.max(previous + step);
        }
        self.stamp = Some(stamp);
        stamp
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uneven_game_cadence_removes_quantization_without_crossing_refresh() {
        let grid = 2155;
        for latency in [3000, 7000] {
            let mut presents = vec![];
            let mut present = 1_000_000;
            for k in 0..600 {
                present += 8000 + (k * 37 % 3000);
                presents.push(present);
            }
            let mut refiner = Refiner::default();
            let (mut error, mut centre_error, mut previous) = (0, 0, 0);
            for (k, &present) in presents.iter().enumerate() {
                let ready = present + latency;
                let composition = (ready + grid - 1) / grid * grid;
                let stamp = refiner.refine(composition, grid, &presents, 12, 100000);
                assert!(stamp > composition - grid && stamp <= composition && stamp > previous);
                previous = stamp;
                if k >= 32 {
                    error += (stamp - ready).abs();
                    centre_error += (composition - grid / 2 - ready).abs();
                }
            }
            assert!(error / 568 < 200);
            assert!(error * 3 < centre_error);
        }
    }
    #[test]
    fn missing_tracking_wrong_refresh_and_source_change_have_bounded_fallbacks() {
        let mut r = Refiner::default();
        assert_eq!(r.refine(10000, 0, &[], 12, 100000), 10000);
        assert_eq!(r.refine(20000, 2000, &[], 12, 100000), 19000);
        r.reset_source();
        for c in [21000, 22000, 23000] {
            r.refine(c, 2000, &[], 12, 100000);
        }
        assert!(r.grid_disabled());
        assert_eq!(r.refine(25000, 2000, &[], 12, 100000), 25000);
        assert_eq!(r.refine(25000, 0, &[], 12, 100000), 25012);
    }
}
