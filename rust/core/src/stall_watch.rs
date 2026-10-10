//! Finds a thread that stopped making progress. The thread marks each phase
//! it enters; a watcher reports once when the newest mark is older than a
//! limit, and once more when the thread marks again, with how long it was
//! stuck and in which phase. Idle phases (waiting for work) never count.

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Stalled { phase: u8, ms: u64 },
    Resumed { phase: u8, ms: u64 },
}

#[derive(Default)]
pub struct Watch {
    /// The mark the thread is stuck at, and its phase.
    stuck: Option<(u64, u8)>,
}
impl Watch {
    /// `at` (the newest mark) and `now` are nanoseconds on one clock.
    pub fn check(&mut self, at: u64, phase: u8, idle: bool, now: u64, limit: u64) -> Option<Event> {
        if let Some((since, stuck_phase)) = self.stuck {
            if at == since {
                return None;
            }
            self.stuck = None;
            return Some(Event::Resumed {
                phase: stuck_phase,
                ms: at.saturating_sub(since) / 1_000_000,
            });
        }
        if idle || now.saturating_sub(at) < limit {
            return None;
        }
        self.stuck = Some((at, phase));
        Some(Event::Stalled {
            phase,
            ms: now.saturating_sub(at) / 1_000_000,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MS: u64 = 1_000_000;

    #[test]
    fn a_stall_is_reported_once_and_its_length_when_the_thread_moves_on() {
        let mut watch = Watch::default();
        assert_eq!(watch.check(100 * MS, 4, false, 400 * MS, 500 * MS), None);
        assert_eq!(
            watch.check(100 * MS, 4, false, 650 * MS, 500 * MS),
            Some(Event::Stalled { phase: 4, ms: 550 })
        );
        assert_eq!(watch.check(100 * MS, 4, false, 4000 * MS, 500 * MS), None);
        // The next mark ends it: stuck from the last mark to this one.
        assert_eq!(
            watch.check(4580 * MS, 1, false, 4600 * MS, 500 * MS),
            Some(Event::Resumed { phase: 4, ms: 4480 })
        );
        assert_eq!(watch.check(4580 * MS, 1, false, 4700 * MS, 500 * MS), None);
        // And a later stall is reported again.
        assert_eq!(
            watch.check(4580 * MS, 2, false, 5100 * MS, 500 * MS),
            Some(Event::Stalled { phase: 2, ms: 520 })
        );
    }

    #[test]
    fn a_thread_waiting_for_work_is_not_stalled() {
        let mut watch = Watch::default();
        assert_eq!(watch.check(0, 0, true, 60_000 * MS, 500 * MS), None);
        assert_eq!(watch.check(0, 0, true, 120_000 * MS, 500 * MS), None);
    }
}
