//! A slow connection retains one pending intra frame per client.
use crate::state::Shared;
use anyhow::{Result, bail};
use butterpollo_core::{
    config::Config,
    packet::{PyrowaveFec, VideoPacketizer},
    pyrowave::DetailFec,
    session::Session,
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
                    while !shared.stop.load(Ordering::Acquire)
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
                        let timestamp = (stamp.saturating_duration_since(start).as_secs_f64() * 90000.)
                            as u64 as u32;
                        let latency = frame
                            .latency
                            .unwrap_or(processing)
                            .as_micros()
                            .min(u128::from(u64::MAX)) as u64;
                        let critical_percentage = config
                            .integer("pyrowave_critical_fec_percentage", 20)
                            .clamp(0, 255)
                            as usize;
                        let requested_fec = config.integer("pyrowave_critical_fec_percentage", 20);
                        if requested_fec != requested_fec.clamp(0, 255) {
                            current.launch.warnings.set("pyrowave_fec_config", format!("PyroWave critical FEC {requested_fec}% is outside 0-255; using {critical_percentage}%. Correct the critical FEC setting."));
                        }
                        let bitrate = current.bitrate.load(Ordering::Relaxed);
                        let kbps = if current.config.configured_bitrate_kbps > 0 {
                            (u64::from(current.config.configured_bitrate_kbps) * u64::from(bitrate)
                                / u64::from(current.config.bitrate_kbps.max(1)))
                            .min(2_000_000) as u32
                        } else {
                            bitrate
                        };
                        let (detail_percentage, wire_budget) =
                            if current.config.pyrowave_records && critical_percentage > 0 {
                                controller.observe(&frame.bytes, now, kbps)?
                            } else {
                                (0, 0)
                            };
                        // Every PyroWave frame stands alone: drop one Moonlight
                        // cannot carry, not the session.
                        let (packet_count, fec_limited, blocks) = match packetizer.pyrowave_blocks(
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
                        ) {
                            Ok(packets) => packets,
                            Err(error) => {
                                current.launch.warnings.set("pyrowave_frame", format!("PyroWave frame dropped ({error:#}); the client may hold the previous picture. Lower bitrate or resolution to stay within Moonlight's packet limit, or use HEVC/AV1."));
                                continue;
                            }
                        };
                        if fec_limited {
                            current.launch.warnings.set("pyrowave_fec_frame", "PyroWave critical FEC was omitted: the coarse picture, parity or packet alignment cannot fit the supported wire blocks. Recovery protection is reduced; restore the default packet size, lower resolution/FEC, or use HEVC/AV1 on a lossy link.");
                        }
                        let overhead = butterpollo_core::network_pacing::overhead(peer.is_ipv6());
                        let packet_bytes = current.config.packet_size
                            + 16
                            + if current.config.encryption & 2 != 0 { 32 } else { 0 };
                        let wire_bytes = packet_count * (packet_bytes + overhead);
                        let demand = (wire_bytes as u64)
                            .saturating_mul(u64::from(current.config.fps_millihz()))
                            / 1000
                            * 8;
                        let bps = butterpollo_core::network_pacing::pyrowave_rate_bps(
                            config.integer("pacing_max_bitrate_kbps", 0),
                            kbps,
                            link,
                            demand,
                        );
                        if Instant::now() >= link_due {
                            butterpollo_core::network_pacing::report_rate(&current.launch.warnings, bps, demand, config.integer("pacing_max_bitrate_kbps", 0));
                        }
                        let dropped = batch.dropped;
                        let mut sent = 0;
                        for packets in blocks {
                            let packets = packets?;
                            let mut remaining = packets.as_slice();
                            while !remaining.is_empty() {
                                if shared.stop.load(Ordering::Acquire) || current.stopping() {
                                    return Ok(());
                                }
                                if pacer.due() > Instant::now() {
                                    timer.until_precise(pacer.due());
                                }
                                let budget =
                                    (bps / 4000).clamp(remaining[0].len() as u64, 64 * 1024) as usize;
                                let count = Batch::count(remaining, budget);
                                let bytes = batch.send(&socket, &remaining[..count], peer)?;
                                sent += bytes;
                                // No catch-up credit after a send waited for room.
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
                        if batch.dropped != dropped {
                            current.launch.warnings.set("network_send", "PyroWave video packets were dropped by the host after transient socket send failures. Lower bitrate and check the network adapter; the log includes the socket error code.");
                        }
                        current.stats.latency_us.store(latency, Ordering::Relaxed);
                        current.stats.frames.fetch_add(1, Ordering::Relaxed);
                        let micros = |d: Duration| d.as_micros().min(u128::from(u64::MAX)) as u64;
                        let sent_at = Instant::now();
                        current.stats.performance.lock().unwrap().record_timing(
                            sent_at,
                            butterpollo_core::performance::Timing {
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
