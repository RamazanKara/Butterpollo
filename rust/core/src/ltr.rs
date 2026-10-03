//! Long-term reference recovery, using Moonlight's one-based wire frame indexes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub mark: Option<usize>,
    pub reference: Option<usize>,
    pub after_invalidation: bool,
    reset: bool,
    next: usize,
}
#[derive(Default)]
pub struct References {
    slots: Vec<Option<u64>>,
    next: usize,
    pending: Option<usize>,
    submitted: u64,
}
impl References {
    pub fn new(count: usize) -> Self {
        Self {
            slots: vec![None; count.min(4)],
            ..Default::default()
        }
    }
    pub fn enabled(&self) -> bool {
        !self.slots.is_empty()
    }
    pub fn disable(&mut self) {
        *self = Self::default();
    }
    pub fn plan(&self, frame: u64, idr: bool) -> Plan {
        let mut plan = Plan {
            mark: None,
            reference: None,
            after_invalidation: false,
            reset: idr,
            next: self.next,
        };
        if !self.enabled() {
            return plan;
        }
        if idr {
            plan.mark = Some(0);
            plan.next = usize::from(self.slots.len() > 1);
        } else if let Some(reference) = self.pending {
            plan.reference = Some(reference);
            plan.after_invalidation = true;
            plan.reset = true;
        } else if frame.is_multiple_of(4) {
            plan.mark = Some(self.next);
            plan.next = if self.slots.len() == 1 {
                0
            } else if self.next + 1 >= self.slots.len() {
                1
            } else {
                self.next + 1
            };
        }
        plan
    }
    /// Commit only once the codec has accepted the input, including when it
    /// applied back pressure before accepting the same frame.
    pub fn accepted(&mut self, frame: u64, plan: &Plan) {
        if plan.reset {
            for (slot, value) in self.slots.iter_mut().enumerate() {
                if Some(slot) != plan.reference {
                    *value = None;
                }
            }
            self.pending = None;
        }
        if let Some(slot) = plan.mark {
            self.slots[slot] = Some(frame);
        }
        self.next = plan.next;
        self.submitted = frame;
    }
    pub fn invalidate(&mut self, first: u64, last: u64) -> bool {
        if first == 0 || first > last || first > self.submitted {
            return false;
        }
        let reference = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(slot, frame)| {
                frame
                    .filter(|frame| *frame < first)
                    .map(|frame| (slot, frame))
            })
            .max_by_key(|(_, frame)| *frame);
        let Some((reference, _)) = reference else {
            return false;
        };
        for frame in &mut self.slots {
            if frame.is_some_and(|frame| frame >= first && frame <= last.max(self.submitted)) {
                *frame = None;
            }
        }
        self.pending = Some(reference);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_loss_feedback_preserves_baseline_and_invalidates_newer_anchors() {
        let mut references = References::new(4);
        let mut marks = vec![];
        for frame in 1..=20 {
            let plan = references.plan(frame, frame == 1);
            if let Some(slot) = plan.mark {
                marks.push((frame, slot));
            }
            references.accepted(frame, &plan);
        }
        assert_eq!(marks, [(1, 0), (4, 1), (8, 2), (12, 3), (16, 1), (20, 2)]);
        assert!(references.invalidate(17, 18));
        let recovery = references.plan(21, false);
        assert_eq!(recovery.reference, Some(1));
        assert!(recovery.after_invalidation);
        assert_eq!(references.plan(21, false), recovery); // unaccepted submit leaves state intact
        references.accepted(21, &recovery);
        assert_eq!(references.slots, [None, Some(16), None, None]);
        assert!(!references.plan(22, false).after_invalidation);
        assert!(!references.invalidate(10, 12));
        let idr = references.plan(22, true);
        references.accepted(22, &idr);
        assert_eq!(references.slots, [Some(22), None, None, None]);
        assert!(!references.invalidate(22, 22));
        assert!(!references.invalidate(24, 23));
    }
    #[test]
    fn single_slot_and_unsupported_drivers_have_bounded_fallbacks() {
        for count in [0, 1, 2, 4, 16] {
            let mut references = References::new(count);
            for frame in 1..=1000 {
                let plan = references.plan(frame, frame == 1);
                assert!(plan.mark.is_none_or(|slot| slot < count.min(4)));
                references.accepted(frame, &plan);
            }
            references.disable();
            assert!(!references.invalidate(900, 910));
            assert!(!references.plan(1001, false).after_invalidation);
        }
    }
}
