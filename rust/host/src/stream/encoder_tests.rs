use super::*;
use butterpollo_windows::encoder::Encoded;
use std::{collections::VecDeque, path::PathBuf};

#[derive(Default)]
struct MockEncoder {
    queued: VecDeque<Encoded>,
    keep_backlog: bool,
}
fn frame(idr: bool) -> Encoded {
    Encoded {
        bytes: vec![0, 0, 0, 1, 0x26, 1, 0x80],
        idr,
        after_invalidation: false,
        latency: None,
        presentation: None,
    }
}
impl EncoderOutput for MockEncoder {
    fn poll(&mut self) -> Result<Vec<Encoded>> {
        let output: Vec<_> = self.queued.pop_front().into_iter().collect();
        if self.keep_backlog && !output.is_empty() {
            self.queued.push_back(frame(false));
        }
        Ok(output)
    }
    fn pending(&self) -> bool {
        !self.queued.is_empty()
    }
    fn backlog(&self) -> usize {
        self.queued.len()
    }
    fn log_stall(&self) {}
}

struct Stream {
    directory: PathBuf,
    recovery: EncoderRecovery,
    encoder: Option<MockEncoder>,
    session: Arc<Session>,
    packetizer: VideoPacketizer,
    sent: Vec<(Instant, bool, Vec<Vec<u8>>)>,
    waits: Vec<Duration>,
}
impl Stream {
    fn new() -> Result<Self> {
        let directory =
            std::env::temp_dir().join(format!("butterpollo-stall-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let launch = Launch {
            id: "stall".into(),
            client: butterpollo_core::state::Client {
                name: "fixture".into(),
                cert: String::new(),
                uuid: "client".into(),
                perm: u32::MAX,
                enabled: true,
                extra: Default::default(),
            },
            peer: "127.0.0.1".parse()?,
            app_id: 1,
            key: [0; 16],
            key_id: 1,
            ping: "ping".into(),
            connect_data: 1,
            role: Role::Stream,
            created: Instant::now(),
            rtsp_encrypted: false,
            rtsp_counter: Default::default(),
            rtsp_received: Default::default(),
            preparation: Default::default(),
            vrr_requested: false,
            host_audio: false,
            requested_rate: 0,
            options: Default::default(),
            audio_preparation: Default::default(),
            preparing: Default::default(),
            warnings: Default::default(),
        };
        Ok(Self {
            recovery: EncoderRecovery {
                stall: crate::soak_fault::EncoderStall::new(Some(directory.clone())),
                ..Default::default()
            },
            directory,
            encoder: Some(MockEncoder::default()),
            session: Session::new(launch, butterpollo_core::rtsp::Negotiated::default()),
            packetizer: VideoPacketizer {
                sequence: 0,
                iv_counter: 0,
                frame: 1,
                packet_size: 1024,
                fec_percent: 0,
                min_fec: 0,
                key: None,
            },
            sent: vec![],
            waits: vec![],
        })
    }
    fn inject(&self, duration: Option<u64>) -> Result<()> {
        match duration {
            Some(ms) => std::fs::write(self.directory.join("encoder stall"), ms.to_string())?,
            None => std::fs::write(self.directory.join("encoder stall.persistent"), [])?,
        }
        Ok(())
    }
    fn step(&mut self, now: Instant) -> Result<()> {
        let result = self.drive(now);
        if result.is_err() {
            self.session.fail();
        }
        result
    }
    fn drive(&mut self, now: Instant) -> Result<()> {
        let output = if self
            .encoder
            .as_ref()
            .is_some_and(|e| self.recovery.backlog(e) >= ENCODER_BACKLOG)
        {
            let since = self.recovery.backlog_since;
            let Some(output) = self.recovery.poll_full(
                &mut self.encoder,
                &self.session.launch.warnings,
                || now,
            )?
            else {
                self.waits.push(now.duration_since(since.unwrap()));
                return Ok(());
            };
            output
        } else {
            self.recovery.backlog_since = None;
            if self.encoder.is_none() {
                self.encoder = Some(MockEncoder::default());
                self.recovery.recreated(&self.session);
            }
            let encoder = self.encoder.as_mut().unwrap();
            encoder
                .queued
                .push_back(frame(self.session.idr.swap(false, Ordering::AcqRel)));
            self.recovery.output(encoder.poll()?, || now)?
        };
        for frame in output {
            let packets = self.packetizer.encode_recovery(
                &frame.bytes,
                frame.idr,
                frame.after_invalidation,
                0,
                0,
            )?;
            self.sent.push((now, frame.idr, packets));
        }
        Ok(())
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
fn a_300_ms_stall_recreates_requests_a_keyframe_and_resumes_sending() -> Result<()> {
    let mut stream = Stream::new()?;
    let start = Instant::now();
    stream.step(start)?;
    stream.inject(Some(300))?;
    for ms in (10..=500).step_by(10) {
        stream.step(start + Duration::from_millis(ms))?;
        if ms < 310 {
            assert_eq!(stream.sent.len(), 1, "no output may escape the fault");
        }
    }
    assert_eq!(stream.waits, [Duration::from_millis(250)]);
    assert_eq!(stream.session.stats.idr_requests.load(Ordering::Relaxed), 1);
    assert!(!stream.session.stopping());
    assert!(stream.recovery.failing.is_none());
    assert!(stream.sent.len() > 3);
    assert_eq!(stream.sent[1].0, start + Duration::from_millis(310));
    assert!(
        stream.sent[1].1,
        "the first recovered picture must be a keyframe"
    );
    assert!(
        stream
            .sent
            .iter()
            .all(|(_, _, packets)| !packets.is_empty())
    );
    assert_eq!(stream.packetizer.frame as usize, stream.sent.len() + 1);
    Ok(())
}

#[test]
fn a_persistent_stall_backs_off_and_fails_the_session_at_the_recovery_budget() -> Result<()> {
    let mut stream = Stream::new()?;
    let start = Instant::now();
    stream.step(start)?;
    stream.inject(None)?;
    let mut ended = None;
    for ms in (10..=21_000).step_by(10) {
        let now = start + Duration::from_millis(ms);
        if let Err(error) = stream.step(now) {
            assert_eq!(
                error.to_string(),
                "the encoder did not produce a frame during recovery"
            );
            ended = Some(now);
            break;
        }
        assert!(!stream.session.stopping());
    }
    let since = stream.recovery.failing.unwrap();
    assert_eq!(ended.unwrap().duration_since(since), ENCODER_RECOVERY);
    assert_eq!(
        &stream.waits[..4],
        &[250, 500, 1000, 2000].map(Duration::from_millis)
    );
    assert!(
        stream.waits[4..]
            .iter()
            .all(|wait| *wait == Duration::from_millis(2000))
    );
    assert_eq!(stream.sent.len(), 1);
    assert!(stream.session.failed());
    assert_eq!(stream.session.termination_reason(), 0x8000_4005);
    Ok(())
}

#[test]
fn a_short_stall_resumes_from_polling_without_recreation() -> Result<()> {
    let mut stream = Stream::new()?;
    let start = Instant::now();
    stream.inject(Some(100))?;
    stream.step(start)?;
    assert!(stream.recovery.pending(stream.encoder.as_ref().unwrap()));
    let before =
        stream
            .recovery
            .collect(&mut stream.encoder, &stream.session.launch.warnings, || {
                start + Duration::from_millis(99)
            })?;
    assert!(before.is_empty());
    let after =
        stream
            .recovery
            .collect(&mut stream.encoder, &stream.session.launch.warnings, || {
                start + Duration::from_millis(100)
            })?;
    assert_eq!(after.len(), 1);
    assert!(after[0].idr);
    assert_eq!(stream.recovery.backlog(stream.encoder.as_ref().unwrap()), 0);
    assert!(stream.waits.is_empty());
    assert!(!stream.session.stopping());
    Ok(())
}

#[test]
fn a_later_stall_starts_again_at_250_ms() -> Result<()> {
    let mut stream = Stream::new()?;
    let start = Instant::now();
    for round in 0..2 {
        stream.inject(Some(300))?;
        for ms in (0..=500).step_by(10) {
            stream.step(start + Duration::from_millis(round * 1000 + ms))?;
        }
        assert!(stream.recovery.failing.is_none());
    }
    assert_eq!(stream.waits, [Duration::from_millis(250); 2]);
    assert_eq!(stream.session.stats.idr_requests.load(Ordering::Relaxed), 2);
    Ok(())
}

#[test]
fn output_resets_the_stall_even_if_the_encoder_backlog_stays_full() -> Result<()> {
    let start = Instant::now();
    let mut recovery = EncoderRecovery {
        failing: Some(start),
        backlog_since: Some(start),
        recreations: 3,
        stall: crate::soak_fault::EncoderStall::new(None),
    };
    let mut encoder = Some(MockEncoder {
        queued: [frame(true), frame(false)].into(),
        keep_backlog: true,
    });
    let output = recovery
        .poll_full(&mut encoder, &Default::default(), || {
            start + Duration::from_millis(100)
        })?
        .unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(recovery.backlog(encoder.as_ref().unwrap()), ENCODER_BACKLOG);
    assert!(recovery.failing.is_none());
    assert!(recovery.backlog_since.is_none());
    recovery.poll_full(&mut encoder, &Default::default(), || {
        start + ENCODER_RECOVERY
    })?;
    assert_eq!(recovery.recreations, 0);
    Ok(())
}

#[test]
fn a_stall_deadline_at_the_budget_keeps_the_existing_terminal_message() -> Result<()> {
    let mut stream = Stream::new()?;
    let start = Instant::now();
    stream.inject(None)?;
    for ms in (0..=300).step_by(10) {
        stream.step(start + Duration::from_millis(ms))?;
    }
    let end = stream.recovery.failing.unwrap() + ENCODER_RECOVERY;
    let error = stream.step(end).unwrap_err();
    assert_eq!(error.to_string(), "the encoder stopped returning frames");
    assert!(stream.session.failed());
    Ok(())
}
