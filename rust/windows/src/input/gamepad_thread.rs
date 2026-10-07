//! The virtual gamepads on their own thread. Every driver call is a blocking
//! DeviceIoControl into a user-mode driver host at normal priority: with the
//! CPUs busy one took up to 365 ms, and keyboard, mouse, touch and the
//! control stream's acknowledgements waited behind it on the input thread.
use super::{Event, Gamepads};
use anyhow::Result;
use butterpollo_core::input::Batch;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// How often feedback is polled and the back button emulated while a pad is
/// plugged in, as the control loop and the C++ host's feedback thread did.
const POLL: Duration = Duration::from_millis(8);
/// How long to wait before opening a driver that failed to open again:
/// opening enumerates devices.
const RETRY: Duration = Duration::from_secs(10);
/// Events waiting for a stalled driver beyond this many are dropped.
const QUEUE_LIMIT: usize = 1024;

/// What the gamepad thread hands back to the control stream.
#[derive(Debug, PartialEq)]
pub enum PadReport {
    /// New rumble, lights or trigger effects for a controller.
    Feedback { id: u16, kind: u16, data: Vec<u8> },
    /// A controller arrived on a profile with motion sensors; the client's
    /// arrival capabilities say which sensors to ask for.
    Motion { id: u8, capabilities: u16 },
}

/// The pads the thread drives: the VHF driver, or a stand-in in tests.
pub(super) trait Pads {
    fn apply(&mut self, event: &Event) -> Result<()>;
    fn refresh(&mut self) -> Result<()>;
    fn feedback(&mut self) -> Result<Vec<(u16, u16, Vec<u8>)>>;
    fn motion_supported(&self, id: u16) -> bool;
    /// Whether any pad is plugged in, and so feedback needs polling.
    fn plugged(&self) -> bool;
}
impl Pads for Gamepads {
    fn apply(&mut self, event: &Event) -> Result<()> {
        Gamepads::apply(self, event)
    }
    fn refresh(&mut self) -> Result<()> {
        Gamepads::refresh(self)
    }
    fn feedback(&mut self) -> Result<Vec<(u16, u16, Vec<u8>)>> {
        Gamepads::feedback(self)
    }
    fn motion_supported(&self, id: u16) -> bool {
        Gamepads::motion_supported(self, id)
    }
    fn plugged(&self) -> bool {
        !self.active.is_empty()
    }
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    stop: AtomicBool,
}
#[derive(Default)]
struct Queue {
    events: VecDeque<Event>,
    /// Whether events were dropped since the thread last took the queue.
    full: bool,
}

/// One client's virtual gamepads, driven on their own thread. The input
/// thread only queues events and collects reports. Dropping it waits for
/// the thread, which unplugs the pads and frees their slots first, so a
/// later session never sees a stale pad.
pub struct GamepadThread {
    shared: Arc<Shared>,
    reports: mpsc::Receiver<PadReport>,
    thread: Option<JoinHandle<()>>,
}
impl GamepadThread {
    /// The driver is opened with the first controller event, on the thread.
    pub fn new(
        profile: u16,
        policy: butterpollo_core::input_policy::Policy,
    ) -> std::io::Result<Self> {
        Self::spawn(move || Gamepads::open_options(profile, policy.clone()))
    }
    pub(super) fn spawn<P: Pads + 'static>(
        open: impl FnMut() -> Result<P> + Send + 'static,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            queue: Mutex::default(),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let (sender, reports) = mpsc::channel();
        let thread = std::thread::Builder::new().name("gamepads".into()).spawn({
            let shared = shared.clone();
            move || run(&shared, open, &sender)
        })?;
        Ok(Self {
            shared,
            reports,
            thread: Some(thread),
        })
    }
    /// Queue an event for the pads; never waits for the driver.
    pub fn send(&self, event: Event) {
        let mut queue = self.shared.queue.lock().unwrap();
        if !enqueue(&mut queue.events, event) && !std::mem::replace(&mut queue.full, true) {
            tracing::warn!(
                queued = QUEUE_LIMIT,
                "virtual gamepad driver is not keeping up; controller input dropped"
            );
        }
        drop(queue);
        self.shared.wake.notify_one();
    }
    /// Feedback and motion requests the thread has produced since the last call.
    pub fn reports(&self) -> impl Iterator<Item = PadReport> + '_ {
        self.reports.try_iter()
    }
}
impl Drop for GamepadThread {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        // Taking the lock orders the flag before a wait that checked it.
        drop(self.shared.queue.lock());
        self.shared.wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Queue `event`, or fold it into a queued event of the same pad as
/// Input::merge allows: a stick or motion update replaces the queued one, so
/// a stalled driver catches up with the latest state, while every button
/// and active-mask change keeps its place. An update may pass over other
/// pads' events and other kinds of event, never over an arrival. Returns
/// false when the event was dropped: QUEUE_LIMIT events already wait.
fn enqueue(events: &mut VecDeque<Event>, event: Event) -> bool {
    if !matches!(event, Event::Arrival { .. }) {
        for queued in events.iter_mut().rev() {
            if matches!(queued, Event::Arrival { .. }) {
                break;
            }
            let same_kind = std::mem::discriminant(&*queued) == std::mem::discriminant(&event);
            match queued.merge(&event) {
                Batch::Merged => return true,
                Batch::Stop if same_kind => break,
                Batch::Skip | Batch::Stop => {}
            }
        }
    }
    if events.len() >= QUEUE_LIMIT {
        return false;
    }
    events.push_back(event);
    true
}

fn run<P: Pads>(
    shared: &Shared,
    mut open: impl FnMut() -> Result<P>,
    reports: &mpsc::Sender<PadReport>,
) {
    let _priority = crate::capture::Priority::input();
    let mut pads: Option<P> = None;
    let mut retry: Option<Instant> = None;
    let mut poll_at = Instant::now();
    // Feedback is polled while a pad is plugged in and once more after the
    // last one leaves, which forgets its last report; otherwise the thread
    // sleeps until an event comes.
    let mut polling = false;
    let stopping = || shared.stop.load(Ordering::Acquire);
    loop {
        let events = {
            let mut queue = shared.queue.lock().unwrap();
            loop {
                if stopping() {
                    // Dropping the pads unplugs them.
                    return;
                }
                if !queue.events.is_empty() {
                    queue.full = false;
                    break std::mem::take(&mut queue.events);
                }
                if !polling && !pads.as_ref().is_some_and(P::plugged) {
                    queue = shared.wake.wait(queue).unwrap();
                    continue;
                }
                let now = Instant::now();
                if now >= poll_at {
                    break VecDeque::new();
                }
                queue = shared.wake.wait_timeout(queue, poll_at - now).unwrap().0;
            }
        };
        for event in events {
            if stopping() {
                return;
            }
            if pads.is_none() {
                let now = Instant::now();
                if retry.is_some_and(|at| now < at) {
                    continue;
                }
                match open() {
                    Ok(opened) => pads = Some(opened),
                    Err(error) => {
                        if retry.is_none() {
                            tracing::warn!(
                                error = format!("{error:#}"),
                                "virtual gamepad driver unavailable; controller input is ignored"
                            );
                        }
                        retry = Some(now + RETRY);
                        continue;
                    }
                }
            }
            let Some(pads) = &mut pads else { continue };
            match pads.apply(&event) {
                Err(error) => tracing::debug!(%error, "input injection failed"),
                Ok(()) => {
                    if let Event::Arrival {
                        id, capabilities, ..
                    } = event
                        && pads.motion_supported(u16::from(id))
                    {
                        let _ = reports.send(PadReport::Motion { id, capabilities });
                    }
                }
            }
        }
        let now = Instant::now();
        if let Some(pads) = &mut pads
            && (polling || pads.plugged())
            && now >= poll_at
        {
            poll_at = now + POLL;
            if let Err(error) = pads.refresh() {
                tracing::debug!(%error, "controller refresh failed");
            }
            for (id, kind, data) in pads.feedback().unwrap_or_default() {
                let _ = reports.send(PadReport::Feedback { id, kind, data });
            }
            polling = pads.plugged();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(id: u16, active: u16, buttons: u32, stick: i16) -> Event {
        Event::Controller {
            id,
            active,
            buttons,
            left_trigger: 0,
            right_trigger: 0,
            sticks: [stick, 0, 0, 0],
        }
    }
    fn motion(id: u8, kind: u8, x: f32) -> Event {
        Event::Motion {
            id,
            kind,
            xyz: [x, 0., 0.],
        }
    }

    /// Holds every driver call until the test opens it.
    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        opened: Condvar,
        /// Calls that reached the driver.
        entered: std::sync::atomic::AtomicUsize,
    }
    impl Gate {
        fn pass(&self) {
            self.entered.fetch_add(1, Ordering::SeqCst);
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.opened.wait(open).unwrap();
            }
        }
        fn open(&self) {
            *self.open.lock().unwrap() = true;
            self.opened.notify_all();
        }
        fn wait_until(&self, mut done: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done() {
                assert!(Instant::now() < deadline, "the gamepad thread stopped");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        fn wait_entered(&self, calls: usize) {
            self.wait_until(|| self.entered.load(Ordering::SeqCst) >= calls);
        }
    }

    /// A driver whose calls wait at a gate, recording what reached it.
    struct Fake {
        gate: Arc<Gate>,
        log: Arc<Mutex<Vec<String>>>,
        plugged: bool,
        feedback: Vec<(u16, u16, Vec<u8>)>,
    }
    impl Pads for Fake {
        fn apply(&mut self, event: &Event) -> Result<()> {
            self.gate.pass();
            self.plugged = true;
            self.log.lock().unwrap().push(format!("{event:?}"));
            Ok(())
        }
        fn refresh(&mut self) -> Result<()> {
            Ok(())
        }
        fn feedback(&mut self) -> Result<Vec<(u16, u16, Vec<u8>)>> {
            self.gate.pass();
            Ok(std::mem::take(&mut self.feedback))
        }
        fn motion_supported(&self, _: u16) -> bool {
            true
        }
        fn plugged(&self) -> bool {
            self.plugged
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            self.log.lock().unwrap().push("unplugged".into());
        }
    }
    fn fake() -> (GamepadThread, Arc<Gate>, Arc<Mutex<Vec<String>>>) {
        let gate = Arc::new(Gate::default());
        let log = Arc::new(Mutex::new(Vec::new()));
        let pads = GamepadThread::spawn({
            let (gate, log) = (gate.clone(), log.clone());
            move || {
                Ok(Fake {
                    gate: gate.clone(),
                    log: log.clone(),
                    plugged: false,
                    feedback: vec![(0, 1, vec![1, 2, 3, 4, 0, 0, 0, 0])],
                })
            }
        })
        .unwrap();
        (pads, gate, log)
    }
    fn logged(events: &[Event]) -> Vec<String> {
        events.iter().map(|event| format!("{event:?}")).collect()
    }

    #[test]
    fn queueing_never_waits_for_a_stalled_driver() {
        let (pads, gate, log) = fake();
        // The first state plugs the pad, and the driver does not return.
        pads.send(state(0, 1, 0, 0));
        gate.wait_entered(1);
        // A press, its release and stick motion meanwhile.
        for i in 1..200 {
            pads.send(state(0, 1, u32::from(i == 50), i));
        }
        // Ending the session waits for the call in progress, not for the
        // queue, and the pad is unplugged before the drop returns.
        pads.shared.stop.store(true, Ordering::Release);
        gate.open();
        drop(pads);
        let mut expected = logged(&[state(0, 1, 0, 0)]);
        expected.push("unplugged".into());
        assert_eq!(*log.lock().unwrap(), expected);
    }

    #[test]
    fn mouse_input_does_not_wait_for_a_stalled_driver() {
        let (pads, gate, _) = fake();
        let mut injector = super::super::Injector::new(r"\\.\DISPLAY99", "vhf").unwrap();
        injector.gamepads = pads;
        let controller = |buttons| Event::Controller {
            id: 0,
            active: 1,
            buttons,
            left_trigger: 0,
            right_trigger: 0,
            sticks: [0; 4],
        };
        injector.apply(&controller(0)).unwrap();
        gate.wait_entered(1);
        // Absolute moves onto a display that does not exist reach no one,
        // but take the same path as every other input; the driver call in
        // progress never returns while they do.
        for buttons in 1..20 {
            injector.apply(&controller(buttons)).unwrap();
            injector
                .apply(&Event::Absolute {
                    x: 10,
                    y: 20,
                    width: 100,
                    height: 100,
                })
                .unwrap();
        }
        assert_eq!(gate.entered.load(Ordering::SeqCst), 1);
        gate.open();
    }

    #[test]
    fn a_stalled_driver_gets_the_latest_state_and_every_press() {
        let (pads, gate, log) = fake();
        // The first state starts a call that waits; the rest queue behind it.
        pads.send(state(0, 1, 0, 0));
        gate.wait_entered(1);
        for stick in 1..=40 {
            pads.send(state(0, 1, 0, stick));
            pads.send(motion(0, 1, f32::from(stick)));
            pads.send(motion(0, 2, f32::from(stick)));
        }
        pads.send(state(0, 1, 1, 41));
        pads.send(state(0, 1, 0, 42));
        for stick in 43..=60 {
            pads.send(state(0, 1, 0, stick));
        }
        gate.open();
        gate.wait_until(|| log.lock().unwrap().len() == 6);
        drop(pads);
        let mut expected = logged(&[
            state(0, 1, 0, 0),
            state(0, 1, 0, 40),
            motion(0, 1, 40.),
            motion(0, 2, 40.),
            state(0, 1, 1, 41),
            state(0, 1, 0, 60),
        ]);
        expected.push("unplugged".into());
        assert_eq!(*log.lock().unwrap(), expected);
    }

    #[test]
    fn feedback_and_motion_are_reported_back() {
        let (pads, gate, _) = fake();
        gate.open();
        pads.send(Event::Arrival {
            id: 2,
            kind: 3,
            capabilities: 0x30,
            buttons: 0,
        });
        let mut reports = Vec::new();
        gate.wait_until(|| {
            reports.extend(pads.reports());
            reports.len() >= 2
        });
        assert_eq!(
            reports,
            [
                PadReport::Motion {
                    id: 2,
                    capabilities: 0x30
                },
                PadReport::Feedback {
                    id: 0,
                    kind: 1,
                    data: vec![1, 2, 3, 4, 0, 0, 0, 0]
                },
            ]
        );
    }
    #[test]
    fn merging_keeps_transitions_arrivals_and_other_pads_in_place() {
        let mut events = VecDeque::new();
        for event in [
            state(0, 3, 0, 1),
            state(1, 3, 0, 1),
            motion(0, 1, 1.),
            state(0, 3, 0, 2), // merges past pad 1 and the motion
            state(1, 3, 2, 2), // pad 1's press keeps its place
            motion(0, 1, 2.),  // merges past both states
            state(0, 1, 0, 3), // pad 1 leaves: a new active mask
            state(0, 1, 0, 4), // merges with the state above only
            Event::Arrival {
                id: 1,
                kind: 1,
                capabilities: 0,
                buttons: 0,
            },
            motion(0, 1, 3.), // never passes the arrival
        ] {
            assert!(enqueue(&mut events, event));
        }
        assert_eq!(
            events,
            [
                state(0, 3, 0, 2),
                state(1, 3, 0, 1),
                motion(0, 1, 2.),
                state(1, 3, 2, 2),
                state(0, 1, 0, 4),
                Event::Arrival {
                    id: 1,
                    kind: 1,
                    capabilities: 0,
                    buttons: 0,
                },
                motion(0, 1, 3.),
            ]
        );
    }

    #[test]
    fn a_full_queue_drops_new_transitions_but_still_merges() {
        let mut events = VecDeque::new();
        for i in 0..QUEUE_LIMIT {
            assert!(enqueue(&mut events, state(0, 1, i as u32, 0)));
        }
        assert!(!enqueue(&mut events, state(0, 1, 1 << 20, 0)));
        assert!(enqueue(&mut events, state(0, 1, QUEUE_LIMIT as u32 - 1, 5)));
        assert_eq!(events.len(), QUEUE_LIMIT);
        assert_eq!(events.back(), Some(&state(0, 1, QUEUE_LIMIT as u32 - 1, 5)));
    }
}
