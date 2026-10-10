//! Ordered conventional video output: one sending frame and one pending frame.
use crate::state::{Session, Shared};
use anyhow::{Result, bail};
use butterpollo_core::{config::Config, packet::VideoPacketizer};
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

struct Frame {
    encoded: Encoded,
    peer: SocketAddr,
    claimed: Instant,
    polled: Instant,
    latency: Duration,
}
struct State {
    pending: Option<Frame>,
    sending: bool,
    next_wire_frame: u32,
    error: Option<String>,
}
impl State {
    fn backlog(&self) -> Result<usize> {
        if let Some(error) = &self.error {
            bail!("video sender stopped: {error}");
        }
        Ok(usize::from(self.sending) + usize::from(self.pending.is_some()))
    }
}
struct Slot {
    state: Mutex<State>,
    changed: Condvar,
    stop: AtomicBool,
    stream_id: String,
}
impl Slot {
    fn run(&self, mut send: impl FnMut(Frame) -> Result<u32>) -> Result<()> {
        loop {
            let frame = {
                let mut state = self.state.lock().unwrap();
                while state.pending.is_none() && !self.stop.load(Ordering::Acquire) {
                    state = self.changed.wait(state).unwrap();
                }
                if self.stop.load(Ordering::Acquire) {
                    return Ok(());
                }
                state.sending = true;
                let frame = state.pending.take().unwrap();
                self.changed.notify_all();
                frame
            };
            let next_wire_frame = send(frame)?;
            let mut state = self.state.lock().unwrap();
            state.next_wire_frame = next_wire_frame;
            state.sending = false;
            self.changed.notify_all();
        }
    }
    fn stop(&self) {
        let _state = self.state.lock().unwrap();
        self.stop.store(true, Ordering::Release);
        self.changed.notify_all();
    }
}
pub struct Sender {
    slot: Arc<Slot>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Sender {
    pub fn new(
        socket: Arc<UdpSocket>,
        s: Arc<Session>,
        c: Config,
        h: Shared,
        start: Instant,
        track_presents: bool,
    ) -> Result<Self> {
        Self::spawn(s.launch.id.clone(), move |shared| {
            let _priority = butterpollo_windows::capture::Priority::new();
            let timer = Timer::new()?;
            let mut packetizer = VideoPacketizer {
                sequence: 0,
                iv_counter: 0,
                frame: 1,
                packet_size: s.config.packet_size,
                fec_percent: c.integer("fec_percentage", 20).clamp(0, 100) as usize,
                min_fec: s.config.min_fec,
                key: if s.config.encryption & 2 != 0 {
                    Some(s.launch.key)
                } else {
                    None
                },
            };
            let mut present_stamper =
                track_presents.then(butterpollo_windows::present_timing::Stamper::default);
            let mut last_stamp = start;
            let period = butterpollo_core::framegen::Rate(s.config.fps_millihz()).period();
            let mut batch = Batch::default();
            let trace_send = tracing::enabled!(target: "pacing", tracing::Level::TRACE);
            batch.waits = trace_send.then(Default::default);
            let mut network_pacer = butterpollo_core::network_pacing::Pacer::new(Instant::now());
            let mut link = None;
            let mut link_due = Instant::now();
            let mut reported_pacing = None;
            let mut fec_reported = None;
            let send_outage = butterpollo_core::stream_policy::SendOutage::from_env();
            let mut send_loss = butterpollo_core::stream_policy::SendLossRecovery::default();
            let batch_kb = match c.integer("video_max_batch_size_kb", 64) {
                16 => 16,
                32 => 32,
                _ => 64,
            };
            shared.run(|queued| {
                let Frame { encoded: frame, peer, claimed, polled, latency: encode } = queued;
                let micros = |d: Duration| d.as_micros().min(u128::from(u64::MAX)) as u64;
                let latency = micros(encode);
                s.stats.latency_us.store(latency, Ordering::Relaxed);
                // Moonlight's host latency runs from the claim to the
                // packet, as the previous host measured it. Waiting
                // before the claim is recorded as frame age.
                let captured = frame.presentation.unwrap_or(claimed);
                let age = micros(claimed.saturating_duration_since(captured));
                let dequeued = Instant::now();
                let queue_wait = micros(dequeued.saturating_duration_since(polled));
                let processing = micros(dequeued.saturating_duration_since(claimed));
                let stamp = present_stamper.as_mut().map_or(captured, |stamper| stamper.stamp(captured, &s.output.read().unwrap())).max(last_stamp + Duration::from_nanos(11_112));
                last_stamp = stamp;
                // Wrap like the previous host; a saturating cast
                // froze the clock after 13.25 hours.
                let timestamp = (stamp.saturating_duration_since(start).as_secs_f64() * 90000.) as u64 as u32;
                if frame.bytes.is_empty() {
                    return Ok(packetizer.frame);
                }
                // A frame beyond Moonlight's packet limit (very high
                // bitrates) costs that frame and a keyframe, not the
                // session.
                let packets = match packetizer.encode_recovery(&frame.bytes,frame.idr,frame.after_invalidation,timestamp,processing) {
                    Ok(packets) => packets,
                    Err(error) => {
                        s.launch.warnings.event("network_frame", format!("Encoded video frame dropped ({error:#}); requesting a recovery frame. Lower bitrate or resolution to stay within Moonlight's packet limit."), butterpollo_core::session::EVENT_PERIOD);
                        s.request_idr();
                        return Ok(packetizer.frame);
                    }
                };
                // A keyframe, as at the start of every stream, may be too
                // large for FEC; only ordinary frames make this worth showing.
                if !frame.idr
                    && packetizer.fec_limited(frame.bytes.len())
                    && fec_reported.is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs(1))
                {
                    fec_reported = Some(Instant::now());
                    s.launch.warnings.event("network_fec", "FEC was reduced or omitted for large video frames because they exceed Moonlight's four-block limit. Packet loss in those frames is harder to recover; lower bitrate or resolution to keep full FEC protection.", butterpollo_core::session::EVENT_PERIOD);
                }
                let frame_bytes = packets.iter().map(|p|p.len() as u64).sum();
                let route = *link.get_or_insert_with(|| butterpollo_windows::net::routed_link(peer));
                let bps = butterpollo_core::network_pacing::rate_bps(
                    c.integer("pacing_max_bitrate_kbps", 0),
                    s.bitrate.load(Ordering::Relaxed),
                    route.bps,
                    route.wireless,
                );
                if Instant::now() >= link_due {
                    let needed = u64::from(s.bitrate.load(Ordering::Relaxed)) * (100 + packetizer.fec_percent as u64) * 10;
                    butterpollo_core::network_pacing::report_rate(&s.launch.warnings, bps, needed, c.integer("pacing_max_bitrate_kbps", 0));
                    if reported_pacing != Some(bps) {
                        tracing::info!(pacing_bps=bps, link_bps=route.bps, configured_kbps=c.integer("pacing_max_bitrate_kbps", 0), "network pacing selected; defaults use twice encoder bitrate for confirmed wireless routes, or the wired fallback ceiling");
                        reported_pacing = Some(bps);
                    }
                }
                let dropped = batch.dropped;
                let waits_before = batch.waits;
                let mut pacing_wait = Duration::ZERO;
                let mut send_time = Duration::ZERO;
                let mut batches = 0;
                let mut first_send = None;
                let mut last_send = None;
                let mut remaining = packets.as_slice();
                let mut lost = butterpollo_core::stream_policy::SendLoss::new(packetizer.block_layout(frame.bytes.len()));
                let mut abandoned = 0;
                while !remaining.is_empty() {
                    if shared.stop.load(Ordering::Acquire) || s.stopping() || h.stop.load(Ordering::Acquire) {
                        shared.stop();
                        return Ok(packetizer.frame);
                    }
                    let now = Instant::now();
                    if network_pacer.due() > now {
                        timer.until_precise(network_pacer.due());
                        if trace_send { pacing_wait += now.elapsed(); }
                    }
                    let budget = (bps / 4000)
                        .clamp(remaining[0].len() as u64, batch_kb * 1024)
                        as usize;
                    let count =
                        butterpollo_windows::net::Batch::count(remaining, budget);
                    let send_started = trace_send.then(Instant::now);
                    let refused = batch.dropped;
                    let bytes = if send_outage.is_some_and(|outage| outage.active(start, Instant::now())) {
                        batch.refuse(count);
                        0
                    } else {
                        batch.send(&socket, &remaining[..count], peer)?
                    };
                    if let Some(send_started) = send_started {
                        let finished = Instant::now();
                        first_send.get_or_insert(send_started);
                        last_send = Some(finished);
                        send_time += finished.duration_since(send_started);
                        batches += 1;
                    }
                    remaining = &remaining[count..];
                    network_pacer.sent(Instant::now(), bytes, if bytes > 0 { count } else { 0 }, peer.ip().to_canonical().is_ipv6(), bps);
                    s.stats.packets.fetch_add(count as u64, Ordering::Relaxed);
                    s.stats.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
                    // As on the inline path: a frame beyond FEC's repair is
                    // not sent further, and the next frame is a keyframe.
                    if lost.sent(count, (batch.dropped - refused) as usize) {
                        abandoned = remaining.len();
                        if send_loss.lost(Instant::now(), period, lost.reached_socket()) {
                            s.request_send_loss_recovery();
                        }
                        break;
                    }
                }
                if lost.unrecoverable() {
                    tracing::debug!(frame = packetizer.frame.wrapping_sub(1), idr = frame.idr, abandoned, "video frame lost on send; the rest of it was not sent");
                } else {
                    send_loss.delivered(frame.idr);
                }
                if batch.dropped != dropped {
                    s.launch.warnings.event("network_send", "Video packets were dropped by the host after transient socket send failures. You may see stutter or recovery frames; lower bitrate and check the network adapter. The log includes the socket error code.", butterpollo_core::session::EVENT_PERIOD);
                }
                s.stats.frames.fetch_add(1, Ordering::Relaxed);
                let sent = Instant::now();
                if let (Some(first), Some(last), Some(before), Some(after)) = (first_send, last_send, waits_before, batch.waits) {
                    tracing::trace!(target: "pacing", frame=packetizer.frame.wrapping_sub(1), packets=packets.len(), batches, queue_wait_us=queue_wait,
                        pacing_wait_us=micros(pacing_wait), send_us=micros(send_time),
                        first_send_us=micros(first.saturating_duration_since(start)), last_send_us=micros(last.saturating_duration_since(start)),
                        writable_waits=after.count-before.count, writable_wait_us=micros(after.elapsed-before.elapsed),
                        dropped=batch.dropped-dropped, stream_id=%s.launch.id, "send");
                }
                s.stats.performance.lock().unwrap().record_timing(sent,butterpollo_core::performance::Timing{period,encode:latency,host:processing,age,sent:micros(sent.saturating_duration_since(claimed))},frame_bytes);
                // The interface lookup takes a moment: refresh the
                // link speed after the frame is out, for the next one.
                if Instant::now() >= link_due {
                    link = Some(butterpollo_windows::net::routed_link(peer));
                    link_due = Instant::now() + Duration::from_secs(2);
                }
                Ok(packetizer.frame)
            })
        })
    }

    fn spawn(
        stream_id: String,
        run: impl FnOnce(&Slot) -> Result<()> + Send + 'static,
    ) -> Result<Self> {
        let slot = Arc::new(Slot {
            state: Mutex::new(State {
                pending: None,
                sending: false,
                next_wire_frame: 1,
                error: None,
            }),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
            stream_id,
        });
        let shared = slot.clone();
        let worker = thread::Builder::new()
            .name("video-send".into())
            .spawn(move || {
                let result = run(&shared);
                let mut state = shared.state.lock().unwrap();
                if let Err(error) = result {
                    state.error = Some(format!("{error:#}"));
                }
                shared.stop.store(true, Ordering::Release);
                state.pending = None;
                state.sending = false;
                shared.changed.notify_all();
            })?;
        Ok(Self {
            slot,
            worker: Some(worker),
        })
    }

    pub fn submit(&self, frames: Vec<Encoded>, peer: SocketAddr, latency: Duration) -> Result<()> {
        let polled = Instant::now();
        let mut state = self.slot.state.lock().unwrap();
        state.backlog()?;
        for encoded in frames {
            let latency = encoded.latency.unwrap_or(latency);
            // Stamp every output from this poll before any capacity wait.
            let claimed = polled.checked_sub(latency).unwrap_or(polled);
            while state.pending.is_some() && !self.slot.stop.load(Ordering::Acquire) {
                state = self.slot.changed.wait(state).unwrap();
                state.backlog()?;
            }
            if self.slot.stop.load(Ordering::Acquire) {
                return Ok(());
            }
            state.pending = Some(Frame {
                encoded,
                peer,
                claimed,
                polled,
                latency,
            });
            tracing::trace!(target: "pacing", pending=usize::from(state.pending.is_some()),
                sending=usize::from(state.sending), inventory=state.backlog()?,
                stream_id=%self.slot.stream_id, "send queue");
            self.slot.changed.notify_all();
        }
        Ok(())
    }
    pub fn backlog(&self) -> Result<usize> {
        self.slot.state.lock().unwrap().backlog()
    }
    /// Recreated encoders must start after every accepted output, including
    /// frames whose packetization has not yet advanced the wire index.
    pub fn next_wire_frame(&self) -> Result<u64> {
        let mut state = self.slot.state.lock().unwrap();
        while state.backlog()? != 0 && !self.slot.stop.load(Ordering::Acquire) {
            state = self.slot.changed.wait(state).unwrap();
        }
        Ok(u64::from(state.next_wire_frame))
    }
}
impl Drop for Sender {
    fn drop(&mut self) {
        self.slot.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests;
