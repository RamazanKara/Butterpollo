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
type Frame = (Encoded, SocketAddr, Duration);
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
                    let mut due = Instant::now();
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
                        let Some((frame, peer, call_latency)) = frame else {
                            continue;
                        };
                        let now = Instant::now();
                        let processing = frame
                            .presentation
                            .map(|captured| now.saturating_duration_since(captured))
                            .unwrap_or_else(|| frame.latency.unwrap_or(call_latency));
                        let captured = frame.presentation.unwrap_or(now);
                        let stamp = stamper
                            .as_mut()
                            .map_or(captured, |stamper| {
                                stamper.stamp(captured, &current.output.read().unwrap())
                            })
                            // Keep at least one 90 kHz RTP tick even without ETW.
                            .max(last_stamp + Duration::from_nanos(11_112));
                        last_stamp = stamp;
                        let timestamp =
                            (stamp.saturating_duration_since(start).as_secs_f64() * 90000.) as u32;
                        let latency = frame
                            .latency
                            .unwrap_or(call_latency)
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
                        let (detail_percentage, wire_budget) =
                            if current.config.pyrowave_records && critical_percentage > 0 {
                                controller.observe(&frame.bytes, now, kbps)?
                            } else {
                                (0, 0)
                            };
                        let packets = packetizer.encode_pyrowave(
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
                        )?;
                        if now >= link_due {
                            link = butterpollo_windows::net::routed_link_bps(peer);
                            link_due = now + Duration::from_secs(2);
                        }
                        let overhead = if peer.is_ipv6() { 86 } else { 66 };
                        let wire_bytes = packets.iter().map(|p| p.len() + overhead).sum::<usize>();
                        let demand = (wire_bytes as u64)
                            .saturating_mul(u64::from(current.config.fps_millihz()))
                            / 1000
                            * 8;
                        let bps = if link > 0 {
                            link * 95 / 100
                        } else {
                            demand.max(u64::from(kbps) * 1100).max(1_000_000)
                        };
                        due = due.max(Instant::now());
                        let mut remaining = packets.as_slice();
                        let mut sent = 0;
                        while !remaining.is_empty() {
                            if shared.stop.load(Ordering::Acquire) || current.stopping() {
                                return Ok(());
                            }
                            if due > Instant::now() {
                                timer.until(due);
                            }
                            let budget =
                                (bps / 4000).clamp(remaining[0].len() as u64, 64 * 1024) as usize;
                            let count = Batch::count(remaining, budget);
                            let bytes = batch.send(&socket, &remaining[..count], peer)?;
                            sent += bytes;
                            due += Duration::from_secs_f64(
                                (bytes + count * overhead) as f64 * 8. / bps as f64,
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
                        current.stats.latency_us.store(latency, Ordering::Relaxed);
                        current.stats.frames.fetch_add(1, Ordering::Relaxed);
                        current.stats.performance.lock().unwrap().record_timing(
                            Instant::now(),
                            latency,
                            processing.as_micros().min(u128::from(u64::MAX)) as u64,
                            sent as u64,
                        );
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
        let mut pending = self.slot.pending.lock().unwrap();
        for frame in frames {
            if pending.replace((frame, peer, latency)).is_some() {
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
