//! Drive the virtual gamepads off the input thread. Blocking VHF driver
//! calls must not delay keyboard, mouse, touch or the control stream's
//! acknowledgements.
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

/// The pads the thread drives: VHF, or a stand-in in tests.
pub(super) trait Pads {
    fn apply(&mut self, event: &Event) -> Result<()>;
    fn refresh(&mut self) -> Result<()>;
    fn feedback(&mut self) -> Vec<(u16, u16, Vec<u8>)>;
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
    fn feedback(&mut self) -> Vec<(u16, u16, Vec<u8>)> {
        Gamepads::feedback(self)
    }
    fn motion_supported(&self, id: u16) -> bool {
        Gamepads::motion_supported(self, id)
    }
    fn plugged(&self) -> bool {
        !self.active.is_empty() || !self.decks.is_empty()
    }
}

struct Shared {
    warnings: Arc<butterpollo_core::session::Warnings>,
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
    /// The back grips no setting maps; pressing one shows a hint.
    unmapped_grips: u32,
}
impl GamepadThread {
    /// The driver is opened with the first controller event, on the thread.
    pub fn new(
        profile: u16,
        policy: butterpollo_core::input_policy::Policy,
        warnings: Arc<butterpollo_core::session::Warnings>,
    ) -> std::io::Result<Self> {
        let unmapped_grips = policy.unmapped_back_grips();
        let pad_warnings = warnings.clone();
        let mut thread = Self::spawn_reported(
            move || Gamepads::open_options(profile, policy.clone(), pad_warnings.clone()),
            warnings,
        )?;
        thread.unmapped_grips = unmapped_grips;
        Ok(thread)
    }
    fn spawn_reported<P: Pads + 'static>(
        open: impl FnMut() -> Result<P> + Send + 'static,
        warnings: Arc<butterpollo_core::session::Warnings>,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            warnings,
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
            unmapped_grips: 0,
        })
    }
    /// Queue an event for the pads; never waits for the driver.
    pub fn send(&self, event: Event) {
        if let Event::Controller { buttons, .. } = &event
            && buttons & self.unmapped_grips != 0
        {
            self.shared.warnings.event(
                "input_back_grips",
                butterpollo_core::input_policy::BACK_GRIP_HINT,
                butterpollo_core::session::EVENT_PERIOD,
            );
        }
        let mut queue = self.shared.queue.lock().unwrap();
        if !enqueue(&mut queue.events, event) && !std::mem::replace(&mut queue.full, true) {
            self.shared.warnings.event("input_gamepad_queue", "Virtual gamepad driver is not keeping up; controller input was dropped. Check the virtual gamepad driver or choose another supported profile.", butterpollo_core::session::EVENT_PERIOD);
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
                    Ok(opened) => {
                        shared.warnings.clear("input_gamepad");
                        pads = Some(opened);
                    }
                    Err(error) => {
                        shared.warnings.set("input_gamepad", format!("Virtual gamepad driver unavailable ({error:#}); controller input is ignored while keyboard and mouse remain available. Run Butterpollo setup again with the gamepad driver selected to restore it."));
                        retry = Some(now + RETRY);
                        continue;
                    }
                }
            }
            let Some(pads) = &mut pads else { continue };
            match pads.apply(&event) {
                Err(error) => shared.warnings.event("input_gamepad_injection", format!("Controller input failed ({error:#}); some controls may not work. Check the selected gamepad profile and virtual driver."), butterpollo_core::session::EVENT_PERIOD),
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
            for (id, kind, data) in pads.feedback() {
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
        fn feedback(&mut self) -> Vec<(u16, u16, Vec<u8>)> {
            self.gate.pass();
            std::mem::take(&mut self.feedback)
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
        let pads = GamepadThread::spawn_reported(
            {
                let (gate, log) = (gate.clone(), log.clone());
                move || {
                    Ok(Fake {
                        gate: gate.clone(),
                        log: log.clone(),
                        plugged: false,
                        feedback: vec![(0, 1, vec![1, 2, 3, 4, 0, 0, 0, 0])],
                    })
                }
            },
            Default::default(),
        )
        .unwrap();
        (pads, gate, log)
    }
    fn logged(events: &[Event]) -> Vec<String> {
        events.iter().map(|event| format!("{event:?}")).collect()
    }

    #[test]
    fn missing_driver_is_visible_without_blocking_other_input() {
        let warnings = Arc::new(butterpollo_core::session::Warnings::default());
        let pads = GamepadThread::spawn_reported::<Fake>(
            || anyhow::bail!("test driver missing"),
            warnings.clone(),
        )
        .unwrap();
        pads.send(state(0, 1, 0, 0));
        let deadline = Instant::now() + Duration::from_secs(2);
        while warnings.snapshot().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let entries = warnings.snapshot();
        assert_eq!(entries[0].code, "input_gamepad");
        assert!(entries[0].message.contains("test driver missing"));
        assert!(
            entries[0]
                .message
                .contains("keyboard and mouse remain available")
        );
    }

    #[test]
    fn an_unmapped_back_grip_press_shows_a_hint_and_still_reaches_the_pads() {
        let warnings = Arc::new(butterpollo_core::session::Warnings::default());
        let gate = Arc::new(Gate::default());
        gate.open();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut pads = GamepadThread::spawn_reported(
            {
                let (gate, log) = (gate.clone(), log.clone());
                move || {
                    Ok(Fake {
                        gate: gate.clone(),
                        log: log.clone(),
                        plugged: false,
                        feedback: vec![],
                    })
                }
            },
            warnings.clone(),
        )
        .unwrap();
        // L4 mapped, R4 not: pressing L4 says nothing.
        pads.unmapped_grips = 0x01_0000;
        pads.send(state(0, 1, 0x02_0000, 0));
        assert!(warnings.snapshot().is_empty());
        pads.send(state(0, 1, 0x01_0000, 0));
        let entries = warnings.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].code, "input_back_grips");
        gate.wait_until(|| log.lock().unwrap().len() == 2);
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
    fn keyboard_and_mouse_do_not_wait_for_a_stalled_driver() {
        static INJECTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let (pads, gate, _) = fake();
        let mut injector = super::super::Injector::new(r"\\.\DISPLAY99", "vhf").unwrap();
        injector.gamepads = pads;
        injector.inject = |inputs| {
            INJECTED.fetch_add(inputs.len(), Ordering::Relaxed);
            inputs.len()
        };
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
        // Nothing reaches the desktop, but keyboard and mouse take the
        // production batching path while the driver call cannot return.
        for buttons in 1..20 {
            assert!(
                injector
                    .apply_all(&[
                        controller(buttons),
                        Event::Keyboard {
                            key: 0x46,
                            down: true,
                            flags: 0,
                            modifiers: 0,
                        },
                        Event::Keyboard {
                            key: 0x46,
                            down: false,
                            flags: 0,
                            modifiers: 0,
                        },
                        Event::Relative { x: 0, y: 0 },
                    ])
                    .is_empty()
            );
        }
        let entered = gate.entered.load(Ordering::SeqCst);
        gate.open();
        assert_eq!(entered, 1);
        assert_eq!(INJECTED.load(Ordering::Relaxed), 19 * 3);
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
