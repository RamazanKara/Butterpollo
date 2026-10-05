//! Bound packet bursts without accumulating a catch-up burst after a late send.
use std::time::{Duration, Instant};

/// Keep the existing 800 Mbps ceiling when the route is unknown or faster.
/// A known Ethernet link gets the same 20% headroom as the gigabit default.
/// Explicit limits retain the stream-bitrate floor, subject to the physical link.
pub fn rate_bps(configured_kbps: i64, stream_kbps: u32, link_bps: u64) -> u64 {
    let requested = if configured_kbps > 0 {
        (configured_kbps as u64)
            .saturating_mul(1000)
            .max(u64::from(stream_kbps) * 1100)
    } else {
        800_000_000
    };
    if link_bps > 0 {
        requested.min(link_bps.saturating_mul(4) / 5).max(1)
    } else {
        requested
    }
}

/// Ethernet framing, inter-packet gap, IP and UDP, beyond the UDP payload.
pub fn overhead(ipv6: bool) -> usize {
    if ipv6 { 86 } else { 66 }
}

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
    /// Called immediately after a batch is sent. Waiting in the socket or
    /// oversleeping must not create credit to burst subsequent batches.
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
        self.due = completed + Duration::from_secs_f64(bytes as f64 * 8. / bps.max(1) as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_routes_bound_default_and_explicit_rates_without_changing_fast_routes() {
        assert_eq!(rate_bps(0, 50_000, 0), 800_000_000);
        assert_eq!(rate_bps(0, 50_000, 2_500_000_000), 800_000_000);
        assert_eq!(rate_bps(0, 50_000, 100_000_000), 80_000_000);
        assert_eq!(rate_bps(30_000, 50_000, 0), 55_000_000);
        assert_eq!(rate_bps(100_000, 100_000, 100_000_000), 80_000_000);
        assert!(rate_bps(i64::MAX, u32::MAX, u64::MAX) > 0);
    }

    #[test]
    fn socket_stalls_and_late_wakeups_cannot_create_catch_up_credit() {
        let start = Instant::now();
        let mut pacer = Pacer::new(start);
        // One millisecond of IPv4 traffic at 8 Mbps.
        pacer.sent(start, 934, 1, false, 8_000_000);
        assert_eq!(pacer.due(), start + Duration::from_millis(1));
        let after_stall = start + Duration::from_millis(6);
        pacer.sent(after_stall, 934, 1, false, 8_000_000);
        assert_eq!(pacer.due(), after_stall + Duration::from_millis(1));
        // Starting the next frame after a long idle period creates no debt.
        let next_frame = start + Duration::from_secs(1);
        pacer.sent(next_frame, 934, 1, false, 8_000_000);
        assert_eq!(pacer.due(), next_frame + Duration::from_millis(1));
    }

    #[test]
    fn each_ipv6_datagram_has_wire_overhead_and_empty_batches_have_no_debt() {
        let start = Instant::now();
        let mut pacer = Pacer::new(start);
        pacer.sent(start, 2 * 914, 2, true, 8_000_000);
        assert_eq!(pacer.due(), start + Duration::from_millis(2));
        pacer.sent(start, 0, 0, false, 8_000_000);
        assert_eq!(pacer.due(), start);
    }
}
