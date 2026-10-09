//! Bounded per-session measurements. No sampling thread or disk writes.
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
struct Frame {
    at: Instant,
    latency: u64,
    processing: u64,
    age: u64,
    sent: u64,
    interval: Option<u64>,
    bytes: u64,
}
struct FecFrame {
    index: u32,
    blocks: u8,
    block_count: u8,
    recovered: bool,
    unrecoverable: bool,
}
/// Microseconds spent on one frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    /// Negotiated frame interval, used to count gaps above twice that interval.
    pub period: Duration,
    /// Claim through completed codec output, including asynchronous work.
    pub encode: u64,
    /// Claim through the pre-packetization sample: host latency sent to Moonlight,
    /// measured as the previous C++ host measured it.
    pub host: u64,
    /// Legacy capture timestamp through the claim. WGC's stamp is not app
    /// Present time and may be clamped to arrival; this is an age estimate.
    /// Moonlight never sees this. Static repeats use a refreshed timestamp.
    pub age: u64,
    /// Claim through sending the final packet, including packetization and pacing.
    pub sent: u64,
}
#[derive(Default)]
pub struct Performance {
    frames: VecDeque<Frame>,
    history: VecDeque<Value>,
    bucket: Option<Instant>,
    count: u64,
    bytes: u64,
    latency: u64,
    maximum: u64,
    processing: u64,
    processing_maximum: u64,
    age: u64,
    send_stutters: u64,
    fec_reports: u64,
    fec_invalid_reports: u64,
    fec_duplicate_reports: u64,
    fec_recovered_frames: u64,
    fec_unrecoverable_frames: u64,
    /// Gaps before each reported highest sequence, not unsent trailing parity.
    fec_missing_packets: u64,
    fec_frames: VecDeque<FecFrame>,
}
impl Performance {
    pub fn record_fec_status(&mut self, payload: &[u8], last_sent: Option<u32>) {
        self.fec_reports = self.fec_reports.saturating_add(1);
        let status = crate::fec_status::Status::parse(payload)
            .ok()
            .zip(last_sent)
            .filter(|(s, last)| last.wrapping_sub(s.frame) < 1024);
        let Some((status, last)) = status else {
            self.fec_invalid_reports = self.fec_invalid_reports.saturating_add(1);
            return;
        };
        // One entry per recent wire frame bounds storage to 1024, even when
        // reports arrive out of order or the u32 frame counter wraps.
        self.fec_frames
            .retain(|f| last.wrapping_sub(f.index) < 1024);
        let index = match self.fec_frames.iter().position(|f| f.index == status.frame) {
            Some(index) => index,
            None => {
                self.fec_frames.push_back(FecFrame {
                    index: status.frame,
                    blocks: 0,
                    block_count: status.blocks,
                    recovered: false,
                    unrecoverable: false,
                });
                self.fec_frames.len() - 1
            }
        };
        let frame = &mut self.fec_frames[index];
        if frame.block_count != status.blocks {
            self.fec_invalid_reports = self.fec_invalid_reports.saturating_add(1);
            return;
        }
        let block = 1 << status.block;
        if frame.blocks & block != 0 {
            self.fec_duplicate_reports = self.fec_duplicate_reports.saturating_add(1);
            return;
        }
        frame.blocks |= block;
        self.fec_missing_packets = self
            .fec_missing_packets
            .saturating_add(u64::from(status.missing_before_highest));
        // These best-effort block reports infer recovery, not successful decode.
        // A later failed block makes the whole reported frame unrecoverable.
        let unrecoverable = u32::from(status.received_data) + u32::from(status.received_parity)
            < u32::from(status.data);
        if unrecoverable && !frame.unrecoverable {
            frame.unrecoverable = true;
            self.fec_unrecoverable_frames = self.fec_unrecoverable_frames.saturating_add(1);
            if frame.recovered {
                self.fec_recovered_frames = self.fec_recovered_frames.saturating_sub(1);
            }
        } else if !unrecoverable
            && status.received_data < status.data
            && !frame.recovered
            && !frame.unrecoverable
        {
            frame.recovered = true;
            self.fec_recovered_frames = self.fec_recovered_frames.saturating_add(1);
        }
    }
    pub fn record(&mut self, now: Instant, latency: u64, bytes: u64) {
        self.record_timing(
            now,
            Timing {
                period: Duration::ZERO,
                encode: latency,
                host: latency,
                age: 0,
                sent: latency,
            },
            bytes,
        );
    }
    pub fn record_timing(&mut self, now: Instant, timing: Timing, bytes: u64) {
        let Timing {
            period,
            encode: latency,
            host: processing,
            age,
            sent,
        } = timing;
        if let Some(start) = self.bucket
            && now.duration_since(start) >= Duration::from_secs(1)
        {
            let elapsed = now.duration_since(start).as_secs_f64();
            let count = self.count.max(1) as f64;
            self.history.push_back(json!({"fps":self.count as f64/elapsed,"bitrate_mbps":self.bytes as f64*8./elapsed/1_000_000.,"encode_mean_ms":self.latency as f64/count/1000.,"encode_max_ms":self.maximum as f64/1000.,"host_processing_mean_ms":self.processing as f64/count/1000.,"host_processing_max_ms":self.processing_maximum as f64/1000.,"frame_age_mean_ms":self.age as f64/count/1000.}));
            if self.history.len() > 120 {
                self.history.pop_front();
            }
            self.bucket = Some(now);
            self.count = 0;
            self.bytes = 0;
            self.latency = 0;
            self.maximum = 0;
            self.processing = 0;
            self.processing_maximum = 0;
            self.age = 0;
        }
        self.bucket.get_or_insert(now);
        self.count += 1;
        self.bytes += bytes;
        self.latency += latency;
        self.maximum = self.maximum.max(latency);
        self.processing += processing;
        self.processing_maximum = self.processing_maximum.max(processing);
        self.age += age;
        let interval = self.frames.back().map(|frame| {
            now.saturating_duration_since(frame.at)
                .as_micros()
                .min(u128::from(u64::MAX)) as u64
        });
        if !period.is_zero() && interval.is_some_and(|us| u128::from(us) > period.as_micros() * 2) {
            self.send_stutters += 1;
        }
        self.frames.push_back(Frame {
            at: now,
            latency,
            processing,
            age,
            sent,
            interval,
            bytes,
        });
        while self.frames.len() > 1024
            || self
                .frames
                .front()
                .is_some_and(|f| now.duration_since(f.at) > Duration::from_secs(2))
        {
            self.frames.pop_front();
        }
    }
    pub fn snapshot(&self, now: Instant) -> Value {
        let frames: Vec<_> = self
            .frames
            .iter()
            .filter(|f| now.duration_since(f.at) <= Duration::from_secs(2))
            .collect();
        let seconds = frames
            .first()
            .map_or(0., |f| now.duration_since(f.at).as_secs_f64());
        let sorted = |value: fn(&Frame) -> u64| {
            let mut values: Vec<_> = frames.iter().map(|f| value(f)).collect();
            values.sort_unstable();
            values
        };
        let latencies = sorted(|f| f.latency);
        let processing = sorted(|f| f.processing);
        let ages = sorted(|f| f.age);
        let present_to_send = sorted(|f| f.age.saturating_add(f.sent));
        let mut intervals: Vec<_> = frames.iter().filter_map(|f| f.interval).collect();
        intervals.sort_unstable();
        let fps = if seconds > 0. {
            frames.len().saturating_sub(1) as f64 / seconds
        } else {
            0.
        };
        let bytes = frames.iter().skip(1).map(|f| f.bytes).sum::<u64>();
        let percentile = |values: &[u64], percent: usize| {
            values
                .get(values.len().saturating_sub(1) * percent / 100)
                .copied()
                .unwrap_or(0) as f64
                / 1000.
        };
        let mean = |values: &[u64]| {
            values.iter().map(|v| *v as f64).sum::<f64>() / values.len().max(1) as f64 / 1000.
        };
        json!({
            "fps":fps,
            "bitrate_mbps":if seconds>0. { bytes as f64*8./seconds/1_000_000. } else { 0. },
            "encode_mean_ms":mean(&latencies),
            "encode_p95_ms":percentile(&latencies,95),
            "encode_p99_ms":percentile(&latencies,99),
            "host_processing_min_ms":percentile(&processing,0),
            "host_processing_mean_ms":mean(&processing),
            "host_processing_max_ms":percentile(&processing,100),
            "host_processing_p95_ms":percentile(&processing,95),
            "host_processing_p99_ms":percentile(&processing,99),
            "frame_age_mean_ms":mean(&ages),
            "frame_age_p95_ms":percentile(&ages,95),
            "frame_age_p99_ms":percentile(&ages,99),
            "present_to_send_mean_ms":mean(&present_to_send),
            "present_to_send_p95_ms":percentile(&present_to_send,95),
            "present_to_send_p99_ms":percentile(&present_to_send,99),
            "send_interval_p95_ms":percentile(&intervals,95),
            "send_interval_p99_ms":percentile(&intervals,99),
            "send_interval_max_ms":percentile(&intervals,100),
            "send_stutters":self.send_stutters,
            "fec_reports":self.fec_reports,
            "fec_invalid_reports":self.fec_invalid_reports,
            "fec_duplicate_reports":self.fec_duplicate_reports,
            "fec_recovered_frames":self.fec_recovered_frames,
            "fec_unrecoverable_frames":self.fec_unrecoverable_frames,
            "fec_missing_packets":self.fec_missing_packets,
            "sample_frames":frames.len(),"history":self.history
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fec(frame: u32, block: u8, data: u16, parity: u16, missing: u16) -> Vec<u8> {
        let mut payload = frame.to_be_bytes().to_vec();
        for word in [11, 0, missing, 10, 2, data, parity] {
            payload.extend(word.to_be_bytes());
        }
        payload.extend([20, block, 2]);
        payload
    }
    #[test]
    fn fec_reports_count_frames_once_and_failed_blocks_override_recovery() {
        let mut p = Performance::default();
        let now = Instant::now();
        let recovered = fec(7, 0, 9, 1, 1);
        p.record_fec_status(&recovered, Some(9));
        p.record_fec_status(&recovered, Some(9));
        assert_eq!(p.snapshot(now)["fec_recovered_frames"], 1);
        p.record_fec_status(&fec(7, 1, 7, 1, 2), Some(9));
        p.record_fec_status(&fec(8, 0, 10, 0, 0), Some(9));
        p.record_fec_status(&fec(9, 1, 8, 2, 2), Some(9));
        p.record_fec_status(&fec(9, 0, 9, 1, 1), Some(9));
        let snapshot = p.snapshot(now);
        assert_eq!(snapshot["fec_reports"], 6);
        assert_eq!(snapshot["fec_invalid_reports"], 0);
        assert_eq!(snapshot["fec_duplicate_reports"], 1);
        assert_eq!(snapshot["fec_recovered_frames"], 1);
        assert_eq!(snapshot["fec_unrecoverable_frames"], 1);
        assert_eq!(snapshot["fec_missing_packets"], 6);
        // Out-of-order reports must not reclassify a lost frame as recovered.
        p.record_fec_status(&fec(10, 1, 7, 1, 2), Some(10));
        p.record_fec_status(&fec(10, 0, 9, 1, 1), Some(10));
        assert_eq!(p.snapshot(now)["fec_recovered_frames"], 1);
        assert_eq!(p.snapshot(now)["fec_unrecoverable_frames"], 2);
        assert_eq!(p.snapshot(now)["sample_frames"], 0);
    }
    #[test]
    fn fec_telemetry_rejects_invalid_unsent_stale_and_inconsistent_reports() {
        let mut p = Performance::default();
        p.record_fec_status(&fec(u32::MAX, 0, 9, 1, 1), Some(0));
        p.record_fec_status(&fec(0, 0, 9, 1, 1), Some(0));
        for (payload, last) in [
            (vec![0; 20], Some(0)),
            (fec(1, 0, 9, 1, 1), Some(0)),
            (fec(1, 0, 9, 1, 1), None),
            (fec(1, 0, 9, 1, 1), Some(1025)),
        ] {
            p.record_fec_status(&payload, last);
        }
        let mut inconsistent = fec(0, 0, 9, 1, 1);
        inconsistent[20] = 1;
        p.record_fec_status(&inconsistent, Some(0));
        assert_eq!(p.fec_invalid_reports, 5);
        assert_eq!(p.fec_recovered_frames, 2);
        assert_eq!(p.fec_missing_packets, 2);
    }
    #[test]
    fn reordered_fec_reports_evict_stale_frames_without_recounting_recent_ones() {
        let mut p = Performance::default();
        p.record_fec_status(&fec(1024, 0, 9, 1, 1), Some(1024));
        for frame in 1..1024 {
            p.record_fec_status(&fec(frame, 0, 10, 0, 0), Some(1024));
        }
        p.record_fec_status(&fec(1025, 0, 10, 0, 0), Some(1025));
        p.record_fec_status(&fec(1024, 0, 9, 1, 1), Some(1025));
        assert_eq!(p.fec_frames.len(), 1024);
        assert_eq!(p.fec_recovered_frames, 1);
        assert_eq!(p.fec_missing_packets, 1);
        assert_eq!(p.fec_duplicate_reports, 1);
    }
    #[test]
    fn fec_history_and_counters_are_bounded() {
        let mut p = Performance::default();
        for frame in 0..1500 {
            p.record_fec_status(&fec(frame, 0, 10, 0, 0), Some(frame));
        }
        assert_eq!(p.fec_frames.len(), 1024);
        assert_eq!(p.fec_recovered_frames, 0);
        assert_eq!(p.fec_missing_packets, 0);
        p.fec_reports = u64::MAX;
        p.fec_invalid_reports = u64::MAX;
        p.record_fec_status(&[], Some(1500));
        assert_eq!(p.fec_reports, u64::MAX);
        assert_eq!(p.fec_invalid_reports, u64::MAX);
        p.fec_missing_packets = u64::MAX;
        p.record_fec_status(&fec(1500, 0, 9, 1, 1), Some(1500));
        assert_eq!(p.fec_missing_packets, u64::MAX);
    }
    fn timing(encode: u64, host: u64, age: u64) -> Timing {
        Timing {
            period: Duration::from_millis(10),
            encode,
            host,
            age,
            sent: host,
        }
    }
    #[test]
    fn present_to_send_includes_packetization_and_pacing_without_changing_wire_latency() {
        let now = Instant::now();
        let mut p = Performance::default();
        p.record_timing(
            now,
            Timing {
                period: Duration::from_millis(10),
                encode: 2000,
                host: 2500,
                age: 4000,
                sent: 12500,
            },
            1000,
        );
        let snapshot = p.snapshot(now);
        assert_eq!(snapshot["host_processing_mean_ms"], 2.5);
        assert_eq!(snapshot["encode_mean_ms"], 2.);
        assert_eq!(snapshot["frame_age_mean_ms"], 4.);
        assert_eq!(snapshot["present_to_send_mean_ms"], 16.5);
        assert_eq!(snapshot["present_to_send_p99_ms"], 16.5);
    }
    #[test]
    fn frame_age_stays_visible_beside_the_reported_host_latency() {
        let start = Instant::now();
        let mut p = Performance::default();
        for i in 0..100 {
            let age = if i % 2 == 0 { 1000 } else { 15000 };
            p.record_timing(
                start + Duration::from_millis(i * 10),
                timing(2000, 2500, age),
                10000,
            );
        }
        let snapshot = p.snapshot(start + Duration::from_millis(990));
        assert_eq!(snapshot["encode_p95_ms"], 2.);
        assert_eq!(snapshot["encode_mean_ms"], 2.);
        assert_eq!(snapshot["host_processing_mean_ms"], 2.5);
        assert_eq!(snapshot["host_processing_max_ms"], 2.5);
        assert_eq!(snapshot["frame_age_mean_ms"], 8.);
        assert_eq!(snapshot["frame_age_p95_ms"], 15.);
        assert_eq!(snapshot["present_to_send_mean_ms"], 10.5);
        assert_eq!(snapshot["present_to_send_p95_ms"], 17.5);
        assert_eq!(snapshot["send_interval_p99_ms"], 10.);
        p.record_timing(start + Duration::from_secs(1), timing(2000, 3000, 0), 10000);
        let snapshot = p.snapshot(start + Duration::from_secs(1));
        assert_eq!(snapshot["history"][0]["host_processing_mean_ms"], 2.5);
        assert_eq!(snapshot["history"][0]["encode_mean_ms"], 2.);
        assert_eq!(snapshot["history"][0]["frame_age_mean_ms"], 8.);
        assert_eq!(
            p.snapshot(start + Duration::from_secs(4))["host_processing_max_ms"],
            0.
        );
    }
    #[test]
    fn completion_intervals_include_stalls_and_small_samples_have_real_extrema() {
        let start = Instant::now();
        let mut p = Performance::default();
        p.record_timing(start, timing(2000, 3000, 1000), 100);
        p.record_timing(
            start + Duration::from_millis(8),
            timing(2000, 4000, 2000),
            100,
        );
        p.record_timing(
            start + Duration::from_millis(32),
            timing(2100, 3000, 900),
            100,
        );
        let snapshot = p.snapshot(start + Duration::from_millis(32));
        assert_eq!(snapshot["send_interval_max_ms"], 24.);
        assert_eq!(snapshot["send_stutters"], 1);
        assert_eq!(snapshot["frame_age_mean_ms"], 1.3);
        assert_eq!(snapshot["host_processing_max_ms"], 4.);
        assert_eq!(
            p.snapshot(start + Duration::from_secs(3))["send_interval_max_ms"],
            0.
        );
        assert_eq!(
            p.snapshot(start + Duration::from_secs(3))["send_stutters"],
            1
        );
    }
    #[test]
    fn stutters_count_only_gaps_above_two_periods_and_survive_window_eviction() {
        let start = Instant::now();
        let mut p = Performance::default();
        for ms in [0, 10, 30, 51] {
            p.record_timing(start + Duration::from_millis(ms), timing(1, 1, 0), 1);
        }
        assert_eq!(
            p.snapshot(start + Duration::from_millis(51))["send_stutters"],
            1
        );
        for i in 1..=3000 {
            p.record_timing(
                start + Duration::from_millis(51 + i * 10),
                timing(1, 1, 0),
                1,
            );
        }
        assert_eq!(
            p.snapshot(start + Duration::from_millis(30051))["send_stutters"],
            1
        );
    }
    #[test]
    fn recent_rates_detect_stalls_and_history_is_bounded() {
        let start = Instant::now();
        let mut p = Performance::default();
        for i in 0..15000 {
            p.record(
                start + Duration::from_millis(i * 10),
                if i % 20 == 0 { 8000 } else { 2000 },
                10000,
            );
        }
        let now = start + Duration::from_millis(149990);
        let s = p.snapshot(now);
        assert!((s["fps"].as_f64().unwrap() - 100.).abs() < 0.1);
        assert_eq!(s["encode_p95_ms"], 2.);
        assert_eq!(s["history"].as_array().unwrap().len(), 120);
        assert!(p.frames.len() <= 1024);
        assert_eq!(p.snapshot(now + Duration::from_secs(3))["fps"], 0.);
    }
}
