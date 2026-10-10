use super::*;
use std::sync::mpsc::{self, Receiver, Sender as Channel};

const WAIT: Duration = Duration::from_secs(5);

fn peer() -> SocketAddr {
    "127.0.0.1:12345".parse().unwrap()
}
fn frame(index: u8) -> Encoded {
    Encoded {
        bytes: vec![index; if index == 1 { 300_000 } else { 3000 }],
        idr: index == 1,
        after_invalidation: index == 3,
        latency: None,
        presentation: None,
    }
}
fn blocked_sender() -> (Sender, Receiver<Frame>, Channel<()>) {
    let (entered, frames) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let sender = Sender::spawn("test".into(), move |slot| {
        slot.run(|frame| {
            let next = u32::from(frame.encoded.bytes[0]) + 1;
            entered.send(frame).unwrap();
            resume.recv_timeout(WAIT)?;
            Ok(next)
        })
    })
    .unwrap();
    (sender, frames, release)
}

#[test]
fn ordered_output_waits_for_the_single_pending_slot_without_replacing_frames() {
    let (sender, frames, release) = blocked_sender();
    sender
        .submit(vec![frame(1)], peer(), Duration::ZERO)
        .unwrap();
    assert!(frames.recv_timeout(WAIT).unwrap().encoded.idr);
    assert_eq!(sender.backlog().unwrap(), 1);
    thread::scope(|scope| {
        let (submitted, done) = mpsc::channel();
        let sender = &sender;
        let producer = scope.spawn(move || {
            let mut third = frame(3);
            third.latency = Some(Duration::from_millis(3));
            sender
                .submit(vec![frame(2), third], peer(), Duration::from_millis(7))
                .unwrap();
            submitted.send(()).unwrap();
        });
        // Wait for the producer to fill the slot, not for thread scheduling.
        {
            let state = sender.slot.state.lock().unwrap();
            let (state, timeout) = sender
                .slot
                .changed
                .wait_timeout_while(state, WAIT, |s| s.pending.is_none())
                .unwrap();
            assert!(!timeout.timed_out());
            assert_eq!(state.pending.as_ref().unwrap().encoded.bytes[0], 2);
            assert_eq!(state.backlog().unwrap(), 2);
        }
        assert!(matches!(done.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let released = Instant::now();
        release.send(()).unwrap();
        let second = frames.recv_timeout(WAIT).unwrap();
        done.recv_timeout(WAIT).unwrap();
        assert_eq!(sender.backlog().unwrap(), 2);
        release.send(()).unwrap();
        let third = frames.recv_timeout(WAIT).unwrap();
        assert_eq!(second.encoded.bytes[0], 2);
        assert_eq!(third.encoded.bytes[0], 3);
        assert!(third.encoded.after_invalidation);
        assert_eq!(second.peer, peer());
        assert_eq!(second.polled, third.polled);
        assert!(third.polled <= released);
        assert_eq!(second.polled - second.claimed, Duration::from_millis(7));
        assert_eq!(third.polled - third.claimed, Duration::from_millis(3));
        release.send(()).unwrap();
        producer.join().unwrap();
    });
    assert_eq!(sender.next_wire_frame().unwrap(), 4);
    assert_eq!(sender.backlog().unwrap(), 0);
}

#[test]
fn recreation_waits_for_sending_and_pending_wire_indices() {
    let (sender, frames, release) = blocked_sender();
    sender
        .submit(vec![frame(1)], peer(), Duration::ZERO)
        .unwrap();
    frames.recv_timeout(WAIT).unwrap();
    sender
        .submit(vec![frame(2)], peer(), Duration::ZERO)
        .unwrap();
    thread::scope(|scope| {
        let (mapped, result) = mpsc::channel();
        let sender = &sender;
        scope.spawn(move || mapped.send(sender.next_wire_frame().unwrap()).unwrap());
        assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release.send(()).unwrap();
        frames.recv_timeout(WAIT).unwrap();
        assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release.send(()).unwrap();
        assert_eq!(result.recv_timeout(WAIT).unwrap(), 3);
    });
}

#[test]
fn a_keyframe_request_skips_the_waiting_frame_and_keeps_its_wire_number() {
    let (entered, frames) = mpsc::channel();
    let (release, resume) = mpsc::channel::<()>();
    let sender = Sender::spawn("skip".into(), move |slot| {
        let mut wire = 1;
        slot.run(|frame| {
            wire += frame.skipped;
            entered
                .send((frame.encoded.bytes[0], wire, frame.skipped))
                .unwrap();
            wire += 1;
            resume.recv_timeout(WAIT)?;
            Ok(wire)
        })
    })
    .unwrap();
    sender
        .submit(vec![frame(1)], peer(), Duration::ZERO)
        .unwrap();
    assert_eq!(frames.recv_timeout(WAIT).unwrap(), (1, 1, 0));
    // A request while a frame waits: it is skipped, the one sending is not.
    sender
        .submit(vec![frame(2)], peer(), Duration::ZERO)
        .unwrap();
    assert_eq!(sender.backlog().unwrap(), 2);
    assert_eq!(sender.skip_pending().unwrap(), 1);
    assert_eq!(sender.skip_pending().unwrap(), 1);
    // A keyframe takes the place of a frame waiting before it.
    sender
        .submit(vec![frame(3)], peer(), Duration::ZERO)
        .unwrap();
    let mut keyframe = frame(4);
    keyframe.idr = true;
    thread::scope(|scope| {
        let (submitted, done) = mpsc::channel();
        let sender = &sender;
        scope.spawn(move || {
            sender
                .submit(vec![keyframe], peer(), Duration::ZERO)
                .unwrap();
            submitted.send(()).unwrap();
        });
        done.recv_timeout(WAIT).unwrap();
    });
    assert_eq!(sender.backlog().unwrap(), 2);
    // A waiting keyframe is never skipped.
    assert_eq!(sender.skip_pending().unwrap(), 2);
    release.send(()).unwrap();
    assert_eq!(frames.recv_timeout(WAIT).unwrap(), (4, 4, 2));
    release.send(()).unwrap();
    assert_eq!(sender.next_wire_frame().unwrap(), 5);
    // A skipped frame with nothing after it still counts for a new encoder.
    sender
        .submit(vec![frame(5)], peer(), Duration::ZERO)
        .unwrap();
    assert_eq!(frames.recv_timeout(WAIT).unwrap(), (5, 5, 0));
    sender
        .submit(vec![frame(6)], peer(), Duration::ZERO)
        .unwrap();
    assert_eq!(sender.skip_pending().unwrap(), 1);
    release.send(()).unwrap();
    assert_eq!(sender.next_wire_frame().unwrap(), 7);
}

#[test]
fn sender_failure_wakes_submit_and_recreation_and_stays_in_the_error_slot() {
    let (entered, sending) = mpsc::channel();
    let (fail, failure) = mpsc::channel();
    let sender = Sender::spawn("error".into(), move |slot| {
        slot.run(|_| {
            entered.send(()).unwrap();
            failure.recv_timeout(WAIT)?;
            bail!("injected socket failure");
        })
    })
    .unwrap();
    sender
        .submit(vec![frame(1)], peer(), Duration::ZERO)
        .unwrap();
    sending.recv_timeout(WAIT).unwrap();
    thread::scope(|scope| {
        let producer =
            scope.spawn(|| sender.submit(vec![frame(2), frame(3)], peer(), Duration::ZERO));
        let recreation = scope.spawn(|| sender.next_wire_frame());
        let state = sender.slot.state.lock().unwrap();
        let (state, timeout) = sender
            .slot
            .changed
            .wait_timeout_while(state, WAIT, |s| s.pending.is_none())
            .unwrap();
        assert!(!timeout.timed_out());
        assert_eq!(state.backlog().unwrap(), 2);
        drop(state);
        fail.send(()).unwrap();
        for error in [
            producer.join().unwrap().unwrap_err(),
            recreation.join().unwrap().unwrap_err(),
            sender.backlog().unwrap_err(),
            sender.submit(vec![], peer(), Duration::ZERO).unwrap_err(),
        ] {
            assert!(error.to_string().contains("injected socket failure"));
        }
    });
    assert!(matches!(
        sending.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
}

#[test]
fn initialization_failure_reaches_the_error_slot_and_reconnect_starts_clean() {
    let mut sender =
        Sender::spawn("init".into(), |_| bail!("timer initialization failed")).unwrap();
    sender.worker.take().unwrap().join().unwrap();
    assert!(
        sender
            .backlog()
            .unwrap_err()
            .to_string()
            .contains("timer initialization failed")
    );
    assert!(
        sender
            .submit(vec![frame(1)], peer(), Duration::ZERO)
            .is_err()
    );
    drop(sender);
    let (sender, frames, release) = blocked_sender();
    assert_eq!(sender.next_wire_frame().unwrap(), 1);
    sender
        .submit(vec![frame(1)], peer(), Duration::ZERO)
        .unwrap();
    assert!(frames.recv_timeout(WAIT).unwrap().encoded.idr);
    release.send(()).unwrap();
    assert_eq!(sender.next_wire_frame().unwrap(), 2);
}

#[test]
fn dropping_an_idle_sender_wakes_and_joins_it() {
    let finished = Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    let sender = Sender::spawn("idle".into(), move |slot| {
        let result = slot.run(|_| panic!("no frame was submitted"));
        done.store(true, Ordering::Release);
        result
    })
    .unwrap();
    drop(sender);
    assert!(finished.load(Ordering::Acquire));
}

#[test]
fn shutdown_interrupts_sending_and_releases_pending_output() {
    let (entered, sending) = mpsc::channel();
    let sender = Sender::spawn("stop".into(), move |slot| {
        slot.run(|frame| {
            entered.send(frame.encoded.bytes[0]).unwrap();
            let state = slot.state.lock().unwrap();
            let (_state, timeout) = slot
                .changed
                .wait_timeout_while(state, WAIT, |_| !slot.stop.load(Ordering::Acquire))
                .unwrap();
            assert!(!timeout.timed_out());
            Ok(2)
        })
    })
    .unwrap();
    sender
        .submit(vec![frame(1)], peer(), Duration::ZERO)
        .unwrap();
    assert_eq!(sending.recv_timeout(WAIT).unwrap(), 1);
    sender
        .submit(vec![frame(2)], peer(), Duration::ZERO)
        .unwrap();
    thread::scope(|scope| {
        let producer = scope.spawn(|| sender.submit(vec![frame(3)], peer(), Duration::ZERO));
        sender.slot.stop();
        producer.join().unwrap().unwrap();
    });
    let slot = sender.slot.clone();
    drop(sender);
    assert!(matches!(
        sending.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
    assert_eq!(slot.state.lock().unwrap().backlog().unwrap(), 0);
}

fn packetizer(key: Option<[u8; 16]>) -> VideoPacketizer {
    VideoPacketizer {
        sequence: 0,
        iv_counter: 0,
        frame: 1,
        packet_size: 1024,
        fec_percent: 20,
        min_fec: 2,
        key,
    }
}

#[test]
fn reconnect_keeps_packet_order_fec_recovery_and_iv_ownership_identical_to_inline() {
    for key in [None, Some([17; 16]), Some([29; 16])] {
        let (sent, packets) = mpsc::channel();
        let sender = Sender::spawn("reconnect".into(), move |slot| {
            let mut packetizer = packetizer(key);
            slot.run(|frame| {
                let frame = frame.encoded;
                sent.send(packetizer.encode_recovery(
                    &frame.bytes,
                    frame.idr,
                    frame.after_invalidation,
                    9000,
                    1234,
                )?)
                .unwrap();
                Ok(packetizer.frame)
            })
        })
        .unwrap();
        assert_eq!(sender.next_wire_frame().unwrap(), 1);
        let mut inline = packetizer(key);
        for index in 1..=4 {
            let expected = frame(index);
            sender
                .submit(vec![frame(index)], peer(), Duration::ZERO)
                .unwrap();
            assert_eq!(
                packets.recv_timeout(WAIT).unwrap(),
                inline
                    .encode_recovery(
                        &expected.bytes,
                        expected.idr,
                        expected.after_invalidation,
                        9000,
                        1234
                    )
                    .unwrap()
            );
        }
        assert_eq!(sender.next_wire_frame().unwrap(), u64::from(inline.frame));
        drop(sender);
        assert!(matches!(
            packets.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }
}
