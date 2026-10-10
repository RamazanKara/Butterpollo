//! Bound packet bursts without accumulating a catch-up burst after a late send.
use std::time::{Duration, Instant};

/// Confirmed wireless routes leave headroom at twice the encoded bitrate.
/// Other routes retain the 800 Mbps ceiling; known Ethernet has 20% headroom.
/// Explicit limits retain the stream-bitrate floor, subject to the physical link.
pub fn rate_bps(configured_kbps: i64, stream_kbps: u32, link_bps: u64, wireless: bool) -> u64 {
    let requested = if configured_kbps > 0 {
        (configured_kbps as u64)
            .saturating_mul(1000)
            .max(u64::from(stream_kbps) * 1100)
    } else if wireless {
        (u64::from(stream_kbps) * 2000).clamp(1_000_000, 800_000_000)
    } else {
        800_000_000
    };
    if link_bps > 0 {
        requested.min(link_bps.saturating_mul(4) / 5).max(1)
    } else {
        requested
    }
}

/// PyroWave uses the available wired link by default, but an explicit cap
/// takes precedence over the bandwidth needed to send every intra frame.
/// Without a known link speed, pace 25% above the frame's wire demand: a
/// budget-sized frame needs about 1.11 times the bitrate on the wire, and
/// pacing at exactly that leaves no room for send calls and timer wake-ups.
pub fn pyrowave_rate_bps(
    configured_kbps: i64,
    stream_kbps: u32,
    link_bps: u64,
    demand_bps: u64,
) -> u64 {
    if configured_kbps > 0 {
        rate_bps(configured_kbps, stream_kbps, link_bps, false)
    } else if link_bps > 0 {
        link_bps.saturating_mul(95) / 100
    } else {
        let needed = demand_bps.max(u64::from(stream_kbps) * 1100);
        (needed.saturating_mul(5) / 4).max(1_000_000)
    }
}

/// Whether video is paced near the stream bitrate: at most four times it,
/// as on a confirmed wireless route (twice), a `pacing_max_bitrate_kbps`
/// cap or a slow wired link. A frame then takes a quarter of a period or
/// more on the wire, and a keyframe several periods.
pub fn paced(bps: u64, stream_kbps: u32) -> bool {
    bps <= u64::from(stream_kbps).saturating_mul(4000)
}

pub fn report_rate(
    warnings: &crate::session::Warnings,
    bps: u64,
    needed_bps: u64,
    configured_kbps: i64,
) {
    if configured_kbps > 0 && bps > (configured_kbps as u64).saturating_mul(1000) {
        warnings.set("network_pacing_floor", format!("Requested pacing cap {configured_kbps} Kbps was raised to {:.1} Mbps by the encoder-bitrate floor. Bursts can exceed the requested cap; lower the encoder bitrate or choose a pacing cap with room for audio and FEC.", bps as f64 / 1_000_000.));
    } else {
        warnings.clear("network_pacing_floor");
    }
    if bps < needed_bps {
        warnings.set("network_pacing", format!("Network pacing is limited to {:.1} Mbps by the configured cap or routed link, below the stream's wire budget. Sending may delay or replace frames; lower client bitrate/FEC or raise the pacing cap only if the network has headroom.", bps as f64 / 1_000_000.));
    } else {
        warnings.clear("network_pacing");
    }
}

/// Ethernet framing, inter-packet gap, IP and UDP, beyond the UDP payload.
pub fn overhead(ipv6: bool) -> usize {
    if ipv6 { 86 } else { 66 }
}

/// Time spent in the send call or waking late that still counts toward the
/// batch's interval. It covers a typical 64 KB send (about 0.2 ms) plus timer
/// lateness; a longer stall or idle gap earns no more than this.
pub const SEND_SLACK: Duration = Duration::from_micros(500);

pub struct Pacer {
    due: Instant,
}
impl Pacer {
    pub fn new(now: Instant) -> Self {
        Self { due: now }
    }
    pub fn due(&self) -> Instant {
        self.due
    }
    /// Called immediately after a batch is sent. Up to `SEND_SLACK` of send
    /// time and timer overshoot consumes the interval; beyond that, a stall or
    /// idle gap earns no catch-up credit, so the next batch waits at least the
    /// interval less `SEND_SLACK`.
    /// Account for bytes on the wire, including one header per datagram.
    pub fn sent(
        &mut self,
        completed: Instant,
        payload: usize,
        packets: usize,
        ipv6: bool,
        bps: u64,
    ) {
        let bytes = payload.saturating_add(packets.saturating_mul(overhead(ipv6)));
        let interval = Duration::from_secs_f64(bytes as f64 * 8. / bps.max(1) as f64);
        self.due = (self.due + interval).max(completed + interval.saturating_sub(SEND_SLACK));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_a_pacing_shortfall_warns_and_a_faster_link_clears_it() {
        let warnings = crate::session::Warnings::default();
        report_rate(
            &warnings,
            rate_bps(0, 100_000, 100_000_000, false),
            120_000_000,
            0,
        );
        assert!(warnings.snapshot()[0].message.contains("80.0 Mbps"));
        report_rate(
            &warnings,
            rate_bps(0, 100_000, 1_000_000_000, false),
            120_000_000,
            0,
        );
        assert!(warnings.snapshot().is_empty());
        report_rate(
            &warnings,
            rate_bps(50_000, 100_000, 0, false),
            100_000_000,
            50_000,
        );
        assert_eq!(warnings.snapshot()[0].code, "network_pacing_floor");
        report_rate(
            &warnings,
            rate_bps(200_000, 100_000, 0, false),
            100_000_000,
            200_000,
        );
        assert!(warnings.snapshot().is_empty());
    }

    #[test]
    fn only_rates_near_the_bitrate_count_as_paced() {
        // Default wired ceiling at the owner's 80 Mb/s: not paced.
        assert!(!paced(rate_bps(0, 80_000, 0, false), 80_000));
        assert!(!paced(rate_bps(0, 80_000, 2_500_000_000, false), 80_000));
        // A 160 Mb/s cap, a wireless route, a 100 Mb/s link: paced.
        assert!(paced(rate_bps(160_000, 80_000, 0, false), 80_000));
        assert!(paced(rate_bps(0, 50_000, 0, true), 50_000));
        assert!(paced(rate_bps(0, 50_000, 100_000_000, false), 50_000));
        assert!(paced(320_000_000, 80_000));
        assert!(!paced(320_000_001, 80_000));
        assert!(!paced(1, 0));
    }

    #[test]
    fn pyrowave_demand_cannot_override_an_explicit_cap_or_link_limit() {
        let demand = 512_000 * 60 * 8;
        assert_eq!(
            pyrowave_rate_bps(30_000, 30_000, 100_000_000, demand),
            33_000_000
        );
        assert_eq!(
            pyrowave_rate_bps(100_000, 100_000, 100_000_000, demand),
            80_000_000
        );
        assert_eq!(
            pyrowave_rate_bps(0, 30_000, 1_000_000_000, demand),
            950_000_000
        );
        assert_eq!(pyrowave_rate_bps(0, 30_000, 0, demand), demand * 5 / 4);
    }

    #[test]
    fn slow_routes_bound_default_and_explicit_rates_without_changing_fast_routes() {
        assert_eq!(rate_bps(0, 50_000, 0, true), 100_000_000);
        assert_eq!(rate_bps(0, 50_000, 0, false), 800_000_000);
        assert_eq!(rate_bps(0, 50_000, 2_500_000_000, false), 800_000_000);
        assert_eq!(rate_bps(0, 50_000, 100_000_000, false), 80_000_000);
        for wireless in [false, true] {
            assert_eq!(rate_bps(30_000, 50_000, 0, wireless), 55_000_000);
            assert_eq!(rate_bps(800_000, 50_000, 0, wireless), 800_000_000);
            assert_eq!(
                rate_bps(100_000, 100_000, 100_000_000, wireless),
                80_000_000
            );
        }
        assert_eq!(rate_bps(0, 1, 0, true), 1_000_000);
        assert_eq!(rate_bps(0, u32::MAX, 0, true), 800_000_000);
        assert!(rate_bps(i64::MAX, u32::MAX, u64::MAX, false) > 0);
    }

    #[test]
    fn wireless_defaults_follow_bitrate_without_capping_pyrowave_demand() {
        for kbps in [9_300, 19_000, 60_000, 149_000] {
            assert_eq!(rate_bps(0, kbps, 0, true), u64::from(kbps) * 2000);
            assert_eq!(rate_bps(0, kbps, 0, false), 800_000_000);
        }
        for kbps in [200_000, 800_000, 2_000_000] {
            let demand = u64::from(kbps) * 1400;
            assert_eq!(pyrowave_rate_bps(0, kbps, 0, demand), demand * 5 / 4);
            assert_eq!(
                pyrowave_rate_bps(0, kbps, 1_000_000_000, demand),
                950_000_000
            );
        }
    }

    /// A record frame of the encoder's full budget, as the host frames it.
    fn budget_frame(width: u32, height: u32, budget: usize) -> Vec<u8> {
        let size = |i: usize| 8 + (i * 73 % 504) * 4;
        let (mut count, mut total) = (0, 8);
        while total + size(count) <= budget {
            total += size(count);
            count += 1;
        }
        let mut raw = Vec::with_capacity(total);
        raw.extend_from_slice(&(0x80000000u32 | (width - 1) | ((height - 1) << 14)).to_le_bytes());
        raw.extend_from_slice(&(count as u32).to_le_bytes());
        for i in 0..count {
            raw.extend_from_slice(&((size(i) as u32 / 4) << 16).to_le_bytes());
            raw.extend_from_slice(&((((i * 37) % count) as u32) << 8).to_le_bytes());
            raw.extend((0..size(i) - 8).map(|j| (i * 31 + j * 17) as u8));
        }
        raw
    }

    #[test]
    fn pyrowave_pacing_leaves_headroom_for_a_budget_sized_frame() {
        use crate::{
            packet::{PyrowaveFec, VideoPacketizer},
            pyrowave,
        };
        // Moonlight's 1392-byte packets less the encryption header.
        let packet_size = 1360;
        let period = Duration::from_secs_f64(1. / 60.);
        for (width, height, kbps) in [
            (1920, 1080, 400_000),
            (2560, 1440, 708_000),
            (3840, 2160, 800_000),
        ] {
            let budget = pyrowave::budget(kbps, period, packet_size, true, true);
            let framed = pyrowave::record_frame(
                &budget_frame(width, height, budget),
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
                key: None,
            };
            let fec = PyrowaveFec {
                records: true,
                critical_percentage: 20,
                detail_percentage: 0,
                wire_budget: 0,
                ipv6: false,
            };
            let (packets, fec_limited, _) = packetizer.pyrowave_blocks(&framed, 0, 0, fec).unwrap();
            assert!(!fec_limited);
            // Payload, RTP and NV headers, encryption, and Ethernet/IP/UDP.
            let wire =
                packets * (pyrowave::aligned_payload(packet_size) + 32 + 32 + overhead(false));
            // As the PyroWave sender computes its demand.
            let demand = (packets * (packet_size + 16 + 32 + overhead(false))) as u64 * 60 * 8;
            let bps = pyrowave_rate_bps(0, kbps, 0, demand);
            let paced = bps as f64 * period.as_secs_f64() / 8.;
            assert!(
                paced >= 1.2 * wire as f64,
                "{width}x{height} at {kbps} Kbps: {packets} packets, {wire} wire bytes, {paced:.0} paced bytes per frame"
            );
        }
    }

    #[test]
    fn socket_time_and_short_wake_delays_do_not_accumulate() {
        let start = Instant::now();
        let mut pacer = Pacer::new(start);
        for batch in 1..=20 {
            let woke = pacer.due() + Duration::from_micros(50);
            let completed = woke + Duration::from_micros(200);
            pacer.sent(completed, 934, 1, false, 8_000_000);
            assert_eq!(pacer.due(), start + Duration::from_millis(batch));
        }
        // Sends slower than the slack fall behind without building debt.
        for _ in 0..5 {
            let completed = pacer.due() + Duration::from_micros(700);
            pacer.sent(completed, 934, 1, false, 8_000_000);
            assert_eq!(
                pacer.due(),
                completed + Duration::from_millis(1) - SEND_SLACK
            );
        }
    }

    #[test]
    fn socket_stalls_and_late_wakeups_cannot_create_catch_up_credit() {
        let start = Instant::now();
        let mut pacer = Pacer::new(start);
        // One millisecond of IPv4 traffic at 8 Mbps.
        let interval = Duration::from_millis(1);
        pacer.sent(start, 934, 1, false, 8_000_000);
        assert_eq!(pacer.due(), start + interval);
        // A 5 ms stall earns only the bounded slack, not a catch-up burst.
        let after_stall = start + Duration::from_millis(6);
        pacer.sent(after_stall, 934, 1, false, 8_000_000);
        assert_eq!(pacer.due(), after_stall + interval - SEND_SLACK);
        let on_time = pacer.due();
        pacer.sent(on_time, 934, 1, false, 8_000_000);
        assert_eq!(pacer.due(), on_time + interval);
        // Starting the next frame after a long idle period creates no debt.
        let next_frame = start + Duration::from_secs(1);
        pacer.sent(next_frame, 934, 1, false, 8_000_000);
        assert_eq!(pacer.due(), next_frame + interval - SEND_SLACK);
    }

    #[test]
    fn each_ipv6_datagram_has_wire_overhead_and_empty_batches_have_no_debt() {
        let start = Instant::now();
        let mut pacer = Pacer::new(start);
        pacer.sent(start, 2 * 914, 2, true, 8_000_000);
        assert_eq!(pacer.due(), start + Duration::from_millis(2));
        let completed = start + Duration::from_millis(3);
        pacer.sent(completed, 0, 0, false, 8_000_000);
        assert_eq!(pacer.due(), completed);
    }
}
