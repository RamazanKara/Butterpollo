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
/// Microseconds spent on one frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
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
}
impl Performance {
    pub fn record(&mut self, now: Instant, latency: u64, bytes: u64) {
        self.record_timing(
            now,
            Timing {
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
            "sample_frames":frames.len(),"history":self.history
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn timing(encode: u64, host: u64, age: u64) -> Timing {
        Timing {
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
        assert_eq!(snapshot["frame_age_mean_ms"], 1.3);
        assert_eq!(snapshot["host_processing_max_ms"], 4.);
        assert_eq!(
            p.snapshot(start + Duration::from_secs(3))["send_interval_max_ms"],
            0.
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
