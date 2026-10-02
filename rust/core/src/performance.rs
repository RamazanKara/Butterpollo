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
    interval: Option<u64>,
    bytes: u64,
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
}
impl Performance {
    pub fn record(&mut self, now: Instant, latency: u64, bytes: u64) {
        self.record_timing(now, latency, latency, bytes);
    }
    /// Capture-to-packetization time is the host latency reported to Moonlight.
    /// Preserve encoder measurements separately so a capture wait stays visible.
    pub fn record_timing(&mut self, now: Instant, latency: u64, processing: u64, bytes: u64) {
        if let Some(start) = self.bucket
            && now.duration_since(start) >= Duration::from_secs(1)
        {
            let elapsed = now.duration_since(start).as_secs_f64();
            self.history.push_back(json!({"fps":self.count as f64/elapsed,"bitrate_mbps":self.bytes as f64*8./elapsed/1_000_000.,"encode_mean_ms":self.latency as f64/self.count.max(1) as f64/1000.,"encode_max_ms":self.maximum as f64/1000.,"host_processing_mean_ms":self.processing as f64/self.count.max(1) as f64/1000.,"host_processing_max_ms":self.processing_maximum as f64/1000.}));
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
        }
        self.bucket.get_or_insert(now);
        self.count += 1;
        self.bytes += bytes;
        self.latency += latency;
        self.maximum = self.maximum.max(latency);
        self.processing += processing;
        self.processing_maximum = self.processing_maximum.max(processing);
        let interval = self.frames.back().map(|frame| {
            now.saturating_duration_since(frame.at)
                .as_micros()
                .min(u128::from(u64::MAX)) as u64
        });
        self.frames.push_back(Frame {
            at: now,
            latency,
            processing,
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
        let mut latencies: Vec<_> = frames.iter().map(|f| f.latency).collect();
        latencies.sort_unstable();
        let fps = if seconds > 0. {
            frames.len().saturating_sub(1) as f64 / seconds
        } else {
            0.
        };
        let bytes = frames.iter().skip(1).map(|f| f.bytes).sum::<u64>();
        let mut processing: Vec<_> = frames.iter().map(|f| f.processing).collect();
        processing.sort_unstable();
        // This difference includes capture age and the small amount of work
        // between retrieving encoder output and preparing its packet header.
        // Keep it explicitly labelled as an estimate, not an acquisition time.
        let mut capture_age: Vec<_> = frames
            .iter()
            .map(|f| f.processing.saturating_sub(f.latency))
            .collect();
        capture_age.sort_unstable();
        let mut intervals: Vec<_> = frames.iter().filter_map(|f| f.interval).collect();
        intervals.sort_unstable();
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
            "encode_p95_ms":percentile(&latencies,95),
            "encode_p99_ms":percentile(&latencies,99),
            "host_processing_min_ms":percentile(&processing,0),
            "host_processing_mean_ms":mean(&processing),
            "host_processing_max_ms":percentile(&processing,100),
            "host_processing_p95_ms":percentile(&processing,95),
            "host_processing_p99_ms":percentile(&processing,99),
            "capture_age_estimate_mean_ms":mean(&capture_age),
            "capture_age_estimate_p95_ms":percentile(&capture_age,95),
            "capture_age_estimate_p99_ms":percentile(&capture_age,99),
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
    #[test]
    fn capture_wait_is_visible_without_changing_encoder_latency() {
        let start = Instant::now();
        let mut p = Performance::default();
        for i in 0..100 {
            p.record_timing(
                start + Duration::from_millis(i * 10),
                2000,
                if i % 2 == 0 { 3000 } else { 17000 },
                10000,
            );
        }
        let snapshot = p.snapshot(start + Duration::from_millis(990));
        assert_eq!(snapshot["encode_p95_ms"], 2.);
        assert_eq!(snapshot["host_processing_min_ms"], 3.);
        assert_eq!(snapshot["host_processing_mean_ms"], 10.);
        assert_eq!(snapshot["host_processing_max_ms"], 17.);
        assert_eq!(snapshot["host_processing_p95_ms"], 17.);
        assert_eq!(snapshot["capture_age_estimate_mean_ms"], 8.);
        assert_eq!(snapshot["capture_age_estimate_p95_ms"], 15.);
        assert_eq!(snapshot["send_interval_p99_ms"], 10.);
        p.record_timing(start + Duration::from_secs(1), 2000, 3000, 10000);
        let snapshot = p.snapshot(start + Duration::from_secs(1));
        assert_eq!(snapshot["history"][0]["host_processing_mean_ms"], 10.);
        assert_eq!(snapshot["history"][0]["encode_mean_ms"], 2.);
        assert_eq!(
            p.snapshot(start + Duration::from_secs(4))["host_processing_max_ms"],
            0.
        );
    }
    #[test]
    fn completion_intervals_include_stalls_and_small_samples_have_real_extrema() {
        let start = Instant::now();
        let mut p = Performance::default();
        p.record_timing(start, 2000, 3000, 100);
        p.record_timing(start + Duration::from_millis(8), 2000, 4000, 100);
        p.record_timing(start + Duration::from_millis(32), 2100, 3000, 100);
        let snapshot = p.snapshot(start + Duration::from_millis(32));
        assert_eq!(snapshot["send_interval_max_ms"], 24.);
        assert_eq!(snapshot["capture_age_estimate_mean_ms"], 1.3);
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
