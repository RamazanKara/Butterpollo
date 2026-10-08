//! A slow connection retains one pending intra frame per client.
use crate::state::{Session, Shared};
use anyhow::{Result, bail};
use butterpollo_core::{
    config::Config,
    packet::{PyrowaveFec, VideoPacketizer},
    pyrowave::DetailFec,
    session::Warnings,
};
use butterpollo_windows::{encoder::Encoded, net::Batch, timing::Timer};
use std::{
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
/// The frame, its destination and when the encoder claimed its capture.
type Frame = (Encoded, SocketAddr, Instant);
#[derive(Default)]
struct DroppedFrames {
    unreported: u64,
    reported: Option<Instant>,
}
impl DroppedFrames {
    fn prepared<T>(
        &mut self,
        result: Result<T>,
        bytes: usize,
        now: Instant,
        warnings: &Warnings,
    ) -> Option<T> {
        match result {
            Ok(packets) => Some(packets),
            Err(error) => {
                warnings.event("pyrowave_frame", format!("PyroWave frame dropped ({error:#}); the client may hold the previous picture. Lower bitrate or resolution to stay within Moonlight's packet limit, or use HEVC/AV1."), butterpollo_core::session::EVENT_PERIOD);
                self.unreported += 1;
                if self
                    .reported
                    .is_none_or(|at| now.duration_since(at) >= Duration::from_secs(5))
                {
                    tracing::warn!(error = %format!("{error:#}"), bytes, dropped = self.unreported, "PyroWave frame dropped");
                    self.unreported = 0;
                    self.reported = Some(now);
                }
                None
            }
        }
    }
}
/// Wire rate to send a frame of `packets` within one frame period.
fn demand_bps(
    packets: usize,
    packet_size: usize,
    encrypted: bool,
    ipv6: bool,
    fps_millihz: u32,
) -> u64 {
    let packet_bytes = packet_size + 16 + if encrypted { 32 } else { 0 };
    let wire_bytes = packets * (packet_bytes + butterpollo_core::network_pacing::overhead(ipv6));
    (wire_bytes as u64).saturating_mul(u64::from(fps_millihz)) / 1000 * 8
}
/// Bytes per socket call: 2 ms at the pacing rate, at least one datagram and
/// at most one 64 KB segmented send.
fn batch_limit(bps: u64, packet: usize) -> usize {
    (bps / 4000).clamp(packet as u64, 64 * 1024) as usize
}
struct Slot {
    pending: Mutex<Option<Frame>>,
    changed: Condvar,
    error: Mutex<Option<String>>,
    stop: AtomicBool,
}
pub struct Sender {
    slot: Arc<Slot>,
    worker: Option<thread::JoinHandle<()>>,
    session: Arc<Session>,
}
impl Sender {
    pub fn new(
        socket: Arc<UdpSocket>,
        session: Arc<Session>,
        config: Config,
        host: Shared,
        start: Instant,
        track_presents: bool,
    ) -> Result<Self> {
        let slot = Arc::new(Slot {
            pending: Mutex::new(None),
            changed: Condvar::new(),
            error: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let shared = slot.clone();
        let current = session.clone();
        let worker = thread::Builder::new()
            .name("pyrowave-send".into())
            .spawn(move || {
                // Like the other media threads: this one paces every packet.
                let _priority = butterpollo_windows::capture::Priority::new();
                let result = (|| -> Result<()> {
                    let timer = Timer::new()?;
                    let mut packetizer = VideoPacketizer {
                        sequence: 0,
                        iv_counter: 0,
                        frame: 1,
                        packet_size: current.config.packet_size,
                        fec_percent: 0,
                        min_fec: current.config.min_fec,
                        key: if current.config.encryption & 2 != 0 {
                            Some(current.launch.key)
                        } else {
                            None
                        },
                    };
                    let mut controller = DetailFec::new(current.config.fps_millihz());
                    let mut stamper =
                        track_presents.then(butterpollo_windows::present_timing::Stamper::default);
                    let mut batch = Batch::default();
                    let mut pacer = butterpollo_core::network_pacing::Pacer::new(Instant::now());
                    let mut last_stamp = start;
                    let mut link_due = start;
                    let mut link = 0;
                    let mut fec_reported: Option<Instant> = None;
                    let requested_fec = config.integer("pyrowave_critical_fec_percentage", 20);
                    if requested_fec != requested_fec.clamp(0, 255) {
                        current.launch.warnings.set("pyrowave_fec_config", format!("PyroWave critical FEC {requested_fec}% is outside 0-255; using {}%. Correct the critical FEC setting.", requested_fec.clamp(0, 255)));
                    }
                    let mut dropped = DroppedFrames::default();
                    'frames: while !shared.stop.load(Ordering::Acquire)
                        && !current.stopping()
                        && !host.stop.load(Ordering::Acquire)
                    {
                        let frame = {
                            let mut guard = shared.pending.lock().unwrap();
                            if guard.is_none() {
                                guard = shared
                                    .changed
                                    .wait_timeout(guard, Duration::from_millis(20))
                                    .unwrap()
                                    .0;
                            }
                            guard.take()
                        };
                        let Some((frame, peer, claimed)) = frame else {
                            continue;
                        };
                        let now = Instant::now();
                        // Moonlight's host latency starts at the claim, as the
                        // previous host measured it; waiting before the claim
                        // is recorded separately as frame age.
                        let processing = now.saturating_duration_since(claimed);
                        let captured = frame.presentation.unwrap_or(claimed);
                        let age = claimed.saturating_duration_since(captured);
                        let stamp = stamper
                            .as_mut()
                            .map_or(captured, |stamper| {
                                stamper.stamp(captured, &current.output.read().unwrap())
                            })
                            // Keep at least one 90 kHz RTP tick even without ETW.
                            .max(last_stamp + Duration::from_nanos(11_112));
                        last_stamp = stamp;
                        let timestamp = (stamp.saturating_duration_since(start).as_secs_f64()
                            * 90000.) as u64 as u32;
                        let latency = frame
                            .latency
                            .unwrap_or(processing)
                            .as_micros()
                            .min(u128::from(u64::MAX)) as u64;
                        let critical_percentage = config
                            .integer("pyrowave_critical_fec_percentage", 20)
                            .clamp(0, 255)
                            as usize;
                        let bitrate = current.bitrate.load(Ordering::Relaxed);
                        let kbps = if current.config.configured_bitrate_kbps > 0 {
                            (u64::from(current.config.configured_bitrate_kbps) * u64::from(bitrate)
                                / u64::from(current.config.bitrate_kbps.max(1)))
                            .min(2_000_000) as u32
                        } else {
                            bitrate
                        };
                        // Every PyroWave frame stands alone: drop one Moonlight
                        // cannot carry, not the session.
                        let Some((detail_percentage, wire_budget)) =
                            (if current.config.pyrowave_records && critical_percentage > 0 {
                                dropped.prepared(
                                    controller.observe(&frame.bytes, now, kbps),
                                    frame.bytes.len(),
                                    Instant::now(),
                                    &current.launch.warnings,
                                )
                            } else {
                                Some((0, 0))
                            })
                        else {
                            continue;
                        };
                        let prepared = packetizer.pyrowave_blocks(
                            &frame.bytes,
                            timestamp,
                            processing.as_micros().min(u128::from(u64::MAX)) as u64,
                            PyrowaveFec {
                                records: current.config.pyrowave_records,
                                critical_percentage,
                                detail_percentage,
                                wire_budget,
                                ipv6: peer.is_ipv6(),
                            },
                        );
                        let Some((packet_count, fec_limited, blocks)) = dropped.prepared(
                            prepared,
                            frame.bytes.len(),
                            Instant::now(),
                            &current.launch.warnings,
                        )
                        else {
                            continue;
                        };
                        if fec_limited
                            && fec_reported.is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
                        {
                            fec_reported = Some(Instant::now());
                            current.launch.warnings.event("pyrowave_fec_frame", "PyroWave critical FEC was omitted: the coarse picture, parity or packet alignment cannot fit the supported wire blocks. Recovery protection is reduced; restore the default packet size, lower resolution/FEC, or use HEVC/AV1 on a lossy link.", butterpollo_core::session::EVENT_PERIOD);
                        }
                        let demand = demand_bps(
                            packet_count,
                            current.config.packet_size,
                            current.config.encryption & 2 != 0,
                            peer.is_ipv6(),
                            current.config.fps_millihz(),
                        );
                        let bps = butterpollo_core::network_pacing::pyrowave_rate_bps(
                            config.integer("pacing_max_bitrate_kbps", 0),
                            kbps,
                            link,
                            demand,
                        );
                        if Instant::now() >= link_due {
                            butterpollo_core::network_pacing::report_rate(&current.launch.warnings, bps, demand, config.integer("pacing_max_bitrate_kbps", 0));
                        }
                        let packets_dropped = batch.dropped;
                        let mut sent = 0;
                        for packets in blocks {
                            let Some(packets) = dropped.prepared(
                                packets,
                                frame.bytes.len(),
                                Instant::now(),
                                &current.launch.warnings,
                            )
                            else {
                                continue 'frames;
                            };
                            let mut remaining = packets.as_slice();
                            while !remaining.is_empty() {
                                if shared.stop.load(Ordering::Acquire) || current.stopping() {
                                    return Ok(());
                                }
                                if pacer.due() > Instant::now() {
                                    timer.until_precise(pacer.due());
                                }
                                let count =
                                    Batch::count(remaining, batch_limit(bps, remaining[0].len()));
                                let bytes = batch.send(&socket, &remaining[..count], peer)?;
                                sent += bytes;
                                // A send that waited for room earns at most SEND_SLACK of credit.
                                pacer.sent(
                                    Instant::now(),
                                    bytes,
                                    if bytes > 0 { count } else { 0 },
                                    peer.is_ipv6(),
                                    bps,
                                );
                                current
                                    .stats
                                    .packets
                                    .fetch_add(count as u64, Ordering::Relaxed);
                                current
                                    .stats
                                    .bytes
                                    .fetch_add(bytes as u64, Ordering::Relaxed);
                                remaining = &remaining[count..];
                            }
                        }
                        if batch.dropped != packets_dropped {
                            current.launch.warnings.event("network_send", "PyroWave video packets were dropped by the host after transient socket send failures. Lower bitrate and check the network adapter; the log includes the socket error code.", butterpollo_core::session::EVENT_PERIOD);
                        }
                        current.stats.latency_us.store(latency, Ordering::Relaxed);
                        current.stats.frames.fetch_add(1, Ordering::Relaxed);
                        let micros = |d: Duration| d.as_micros().min(u128::from(u64::MAX)) as u64;
                        let sent_at = Instant::now();
                        current.stats.performance.lock().unwrap().record_timing(
                            sent_at,
                            butterpollo_core::performance::Timing {
                                period: Duration::from_secs_f64(1000. / f64::from(current.config.fps_millihz())),
                                encode: latency,
                                host: micros(processing),
                                age: micros(age),
                                sent: micros(sent_at.saturating_duration_since(claimed)),
                            },
                            sent as u64,
                        );
                        // The interface lookup takes a moment: refresh the link
                        // speed after the frame is out, for the next one.
                        if Instant::now() >= link_due {
                            link = butterpollo_windows::net::routed_link_bps(peer);
                            link_due = Instant::now() + Duration::from_secs(2);
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    *shared.error.lock().unwrap() = Some(format!("{error:#}"));
                    shared.changed.notify_all();
                }
            })?;
        Ok(Self {
            slot,
            worker: Some(worker),
            session,
        })
    }
    pub fn submit(&self, frames: Vec<Encoded>, peer: SocketAddr, latency: Duration) -> Result<()> {
        if let Some(error) = self.slot.error.lock().unwrap().as_ref() {
            bail!("video sender stopped: {error}");
        }
        if frames.is_empty() {
            return Ok(());
        }
        let polled = Instant::now();
        let mut pending = self.slot.pending.lock().unwrap();
        for frame in frames {
            let claimed = polled
                .checked_sub(frame.latency.unwrap_or(latency))
                .unwrap_or(polled);
            if pending.replace((frame, peer, claimed)).is_some() {
                self.session
                    .stats
                    .frames_replaced
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        self.slot.changed.notify_one();
        Ok(())
    }
}
impl Drop for Sender {
    fn drop(&mut self) {
        self.slot.stop.store(true, Ordering::Release);
        self.slot.changed.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_block_abandons_the_frame_and_the_next_frame_keeps_its_counters() {
        for fail_at in [0, 1] {
            let payload = vec![7; 400_000];
            let mut packetizer = VideoPacketizer {
                sequence: u32::MAX - 10,
                iv_counter: 100,
                frame: u32::MAX,
                packet_size: 1392,
                fec_percent: 0,
                min_fec: 2,
                key: Some([42; 16]),
            };
            let fec = || PyrowaveFec {
                records: false,
                critical_percentage: 20,
                detail_percentage: 0,
                wire_budget: 0,
                ipv6: false,
            };
            let mut dropped = DroppedFrames::default();
            let warnings = Warnings::default();
            let now = Instant::now();
            let mut sent = 0;
            {
                let (_, _, mut blocks) = packetizer
                    .pyrowave_blocks(&payload, 9000, 543, fec())
                    .unwrap();
                let mut prepared = 0;
                let failing = std::iter::from_fn(|| {
                    let result = if prepared == fail_at {
                        Some(Err(anyhow::anyhow!("injected block preparation failure")))
                    } else {
                        blocks.next()
                    };
                    prepared += 1;
                    result
                });
                for block in failing {
                    let Some(packets) = dropped.prepared(block, payload.len(), now, &warnings)
                    else {
                        break;
                    };
                    sent += packets.len();
                }
                assert_eq!(prepared, fail_at + 1);
            }
            assert_eq!(warnings.snapshot()[0].code, "pyrowave_frame");
            let sequence = (u32::MAX - 10).wrapping_add(sent as u32);
            let frame = if fail_at == 0 { u32::MAX } else { 0 };
            assert_eq!(packetizer.sequence, sequence);
            assert_eq!(packetizer.iv_counter, 100 + sent as u64);
            assert_eq!(packetizer.frame, frame);
            let packets = dropped
                .prepared(
                    packetizer.encode_pyrowave(&payload, 9750, 543, fec()),
                    payload.len(),
                    now,
                    &warnings,
                )
                .unwrap();
            assert_eq!(&packets[0][..8], &(100 + sent as u64).to_le_bytes());
            assert_eq!(&packets[0][12..16], &frame.to_le_bytes());
            assert_eq!(
                packetizer.sequence,
                sequence.wrapping_add(packets.len() as u32)
            );
            assert_eq!(packetizer.iv_counter, 100 + (sent + packets.len()) as u64);
            assert_eq!(packetizer.frame, frame.wrapping_add(1));
        }
    }

    #[test]
    fn malformed_frame_after_a_late_interval_is_dropped_by_detail_planning() {
        // Detail planning parses records only after a late frame. That parse
        // used to end the session through `?` before block preparation ran.
        let mut controller = DetailFec::new(60_000);
        let mut dropped = DroppedFrames::default();
        let warnings = Warnings::default();
        let now = Instant::now();
        let malformed = [0; 64];
        assert_eq!(
            dropped.prepared(
                controller.observe(&malformed, now, 100_000),
                64,
                now,
                &warnings
            ),
            Some((0, 0))
        );
        assert!(warnings.snapshot().is_empty());
        let late = now + Duration::from_millis(50);
        assert!(
            dropped
                .prepared(
                    controller.observe(&malformed, late, 100_000),
                    64,
                    late,
                    &warnings
                )
                .is_none()
        );
        assert_eq!(dropped.reported, Some(late));
        assert_eq!(warnings.snapshot()[0].code, "pyrowave_frame");
    }

    #[test]
    fn frame_drop_warnings_report_the_first_failure_and_count_repeats() {
        let mut dropped = DroppedFrames::default();
        let warnings = Warnings::default();
        let now = Instant::now();
        for (millis, unreported) in [(0, 0), (1, 1), (4999, 2), (5000, 0)] {
            warnings.clear("pyrowave_frame");
            assert!(
                dropped
                    .prepared::<()>(
                        Err(anyhow::anyhow!("bad frame")),
                        123,
                        now + Duration::from_millis(millis),
                        &warnings,
                    )
                    .is_none()
            );
            assert_eq!(dropped.unreported, unreported);
            assert_eq!(warnings.snapshot()[0].code, "pyrowave_frame");
            if millis == 0 || millis == 5000 {
                assert_eq!(dropped.reported, Some(now + Duration::from_millis(millis)));
            }
        }
        assert_eq!(
            dropped.prepared(Ok(42), 123, now + Duration::from_secs(6), &warnings),
            Some(42)
        );
        assert_eq!(dropped.reported, Some(now + Duration::from_secs(5)));
    }

    /// An encrypted 1080p60 frame filling the encoder's 400 Mbps budget, as
    /// the sender's blocks, and the rate the sender paces it at.
    fn budget_sized_1080p_frame() -> (Vec<Vec<Vec<u8>>>, u64, Duration) {
        use butterpollo_core::pyrowave;
        let (packet_size, kbps, fps_millihz) = (1360, 400_000, 60_000);
        let period = Duration::from_secs_f64(1000. / f64::from(fps_millihz));
        let budget = pyrowave::budget(kbps, period, packet_size, true, true);
        let size = |i: usize| 8 + (i * 73 % 504) * 4;
        let (mut count, mut total) = (0, 8);
        while total + size(count) <= budget {
            total += size(count);
            count += 1;
        }
        let mut raw = Vec::with_capacity(total);
        raw.extend_from_slice(&(0x80000000u32 | 1919 | (1079 << 14)).to_le_bytes());
        raw.extend_from_slice(&(count as u32).to_le_bytes());
        for i in 0..count {
            raw.extend_from_slice(&((size(i) as u32 / 4) << 16).to_le_bytes());
            raw.extend_from_slice(&((((i * 37) % count) as u32) << 8).to_le_bytes());
            raw.extend((0..size(i) - 8).map(|j| (i * 31 + j * 17) as u8));
        }
        let framed = pyrowave::record_frame(
            &raw,
            pyrowave::aligned_payload(packet_size),
            pyrowave::max_frame_bytes(packet_size, true),
        )
        .unwrap();
        let mut packetizer = VideoPacketizer {
            sequence: 0,
            iv_counter: 0,
            frame: 1,
            packet_size,
            fec_percent: 0,
            min_fec: 2,
            key: Some([42; 16]),
        };
        let fec = PyrowaveFec {
            records: true,
            critical_percentage: 20,
            detail_percentage: 0,
            wire_budget: 0,
            ipv6: false,
        };
        let (packets, _, blocks) = packetizer.pyrowave_blocks(&framed, 0, 0, fec).unwrap();
        let blocks = blocks.collect::<Result<Vec<_>>>().unwrap();
        let demand = demand_bps(packets, packet_size, true, false, fps_millihz);
        let bps = butterpollo_core::network_pacing::pyrowave_rate_bps(0, kbps, 0, demand);
        (blocks, bps, period)
    }

    #[test]
    fn paced_budget_sized_1080p_frame_leaves_within_85_percent_of_the_period() {
        let (blocks, bps, period) = budget_sized_1080p_frame();
        // Loopback costs on the RX 7900 XT machine: a 64 KB segmented send
        // takes about 0.2 ms and the precise timer wakes up to 0.1 ms late.
        let (send, wake) = (Duration::from_micros(250), Duration::from_micros(100));
        let start = Instant::now();
        let mut pacer = butterpollo_core::network_pacing::Pacer::new(start);
        let mut now = start;
        let mut batches = 0;
        for block in &blocks {
            let mut remaining = block.as_slice();
            while !remaining.is_empty() {
                if pacer.due() > now {
                    now = pacer.due() + wake;
                }
                let count = Batch::count(remaining, batch_limit(bps, remaining[0].len()));
                let bytes = remaining[..count].iter().map(Vec::len).sum();
                now += send;
                pacer.sent(now, bytes, count, false, bps);
                remaining = &remaining[count..];
                batches += 1;
            }
        }
        let spent = now - start;
        assert!(
            spent <= period.mul_f64(0.85),
            "{batches} batches at {bps} bps took {spent:?} of a {period:?} frame"
        );
    }

    /// Timing-dependent: run at normal priority on an otherwise idle machine
    /// with `cargo test -p butterpollo pyrowave_loopback -- --ignored --nocapture`.
    #[test]
    #[ignore = "measures real loopback sends; needs an idle machine"]
    fn pyrowave_loopback_sends_a_budget_sized_1080p_frame_within_85_percent_of_the_period()
    -> Result<()> {
        let (blocks, bps, period) = budget_sized_1080p_frame();
        let receiver = UdpSocket::bind("127.0.0.1:0")?;
        socket2::SockRef::from(&receiver).set_recv_buffer_size(64 << 20)?;
        receiver.set_read_timeout(Some(Duration::from_millis(50)))?;
        let peer = receiver.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let drain = {
            let stop = stop.clone();
            thread::spawn(move || {
                let mut buffer = [0; 2048];
                while !stop.load(Ordering::Relaxed) {
                    let _ = receiver.recv(&mut buffer);
                }
            })
        };
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        socket.set_nonblocking(true)?;
        butterpollo_windows::net::configure_udp(&socket)?;
        let timer = Timer::new()?;
        let mut batch = Batch::default();
        let mut pacer = butterpollo_core::network_pacing::Pacer::new(Instant::now());
        let mut spans = Vec::new();
        for _ in 0..180 {
            let begin = Instant::now();
            for block in &blocks {
                let mut remaining = block.as_slice();
                while !remaining.is_empty() {
                    if pacer.due() > Instant::now() {
                        timer.until_precise(pacer.due());
                    }
                    let count = Batch::count(remaining, batch_limit(bps, remaining[0].len()));
                    let bytes = batch.send(&socket, &remaining[..count], peer)?;
                    pacer.sent(Instant::now(), bytes, count, false, bps);
                    remaining = &remaining[count..];
                }
            }
            spans.push(begin.elapsed());
            timer.until_precise(begin + period);
        }
        stop.store(true, Ordering::Relaxed);
        drain.join().unwrap();
        spans.sort();
        let median = spans[spans.len() / 2];
        println!(
            "{bps} bps, frame send median {median:?}, p95 {:?}, max {:?}, period {period:?}, dropped {}",
            spans[spans.len() * 95 / 100],
            spans[spans.len() - 1],
            batch.dropped
        );
        assert!(median <= period.mul_f64(0.85));
        Ok(())
    }
}
