//! Bounded per-session measurements. No sampling thread or disk writes.
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
struct Frame {
    at: Instant,
    latency: u64,
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
}
impl Performance {
    pub fn record(&mut self, now: Instant, latency: u64, bytes: u64) {
        if let Some(start) = self.bucket
            && now.duration_since(start) >= Duration::from_secs(1)
        {
            let elapsed = now.duration_since(start).as_secs_f64();
            self.history.push_back(json!({"fps":self.count as f64/elapsed,"bitrate_mbps":self.bytes as f64*8./elapsed/1_000_000.,"encode_mean_ms":self.latency as f64/self.count.max(1) as f64/1000.,"encode_max_ms":self.maximum as f64/1000.}));
            if self.history.len() > 120 {
                self.history.pop_front();
            }
            self.bucket = Some(now);
            self.count = 0;
            self.bytes = 0;
            self.latency = 0;
            self.maximum = 0;
        }
        self.bucket.get_or_insert(now);
        self.count += 1;
        self.bytes += bytes;
        self.latency += latency;
        self.maximum = self.maximum.max(latency);
        self.frames.push_back(Frame {
            at: now,
            latency,
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
        json!({"fps":fps,"bitrate_mbps":if seconds>0. { bytes as f64*8./seconds/1_000_000. } else { 0. },"encode_p95_ms":latencies.get(latencies.len().saturating_sub(1)*95/100).copied().unwrap_or(0) as f64/1000.,"sample_frames":frames.len(),"history":self.history})
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
