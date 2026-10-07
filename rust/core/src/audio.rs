//! Bounded audio buffering, speaker mapping and fractional resampling.
use anyhow::{Result, bail};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The layouts and bitrates advertised by the retained Moonlight protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpusLayout {
    pub channels: usize,
    pub streams: i32,
    pub coupled: i32,
    pub mapping: Vec<u8>,
    pub bitrate: i32,
}
impl OpusLayout {
    pub fn select(channels: usize, quality: bool, custom: Option<&str>) -> Result<Self> {
        let (streams, coupled, mapping, bitrate) = match (channels, quality) {
            (2, false) => (1, 1, vec![0, 1], 96000),
            (2, true) => (1, 1, vec![0, 1], 512000),
            (6, false) => (4, 2, vec![0, 1, 4, 5, 2, 3], 256000),
            (6, true) => (6, 0, vec![0, 1, 4, 5, 2, 3], 1536000),
            (8, false) => (5, 3, vec![0, 1, 4, 5, 6, 7, 2, 3], 450000),
            (8, true) => (8, 0, vec![0, 1, 4, 5, 6, 7, 2, 3], 2048000),
            _ => bail!("unsupported Opus layout"),
        };
        let mut layout = Self {
            channels,
            streams,
            coupled,
            mapping,
            bitrate,
        };
        if let Some((streams, coupled, mapping)) =
            custom.and_then(|s| Self::parse_custom(channels, s))
        {
            layout.streams = streams;
            layout.coupled = coupled;
            layout.mapping = mapping;
        }
        Ok(layout)
    }
    fn parse_custom(channels: usize, text: &str) -> Option<(i32, i32, Vec<u8>)> {
        let b = text.as_bytes();
        if !matches!(channels, 6 | 8)
            || b.len() != channels + 3
            || !b.iter().all(u8::is_ascii_digit)
        {
            return None;
        }
        let c = usize::from(b[0] - b'0');
        let streams = i32::from(b[1] - b'0');
        let coupled = i32::from(b[2] - b'0');
        let mapping: Vec<_> = b[3..].iter().map(|v| v - b'0').collect();
        if c != channels
            || streams + coupled != channels as i32
            || coupled > streams
            || streams == 0
            || mapping.iter().any(|v| usize::from(*v) >= channels)
        {
            return None;
        }
        Some((streams, coupled, mapping))
    }
    pub fn valid_custom(text: &str) -> bool {
        matches!(text.as_bytes().first(), Some(b'6' | b'8'))
            && Self::parse_custom(usize::from(text.as_bytes()[0] - b'0'), text).is_some()
    }
}

pub struct Resampler {
    queue: VecDeque<f32>,
    channels: usize,
    rate: u32,
    phase: f64,
}
impl Resampler {
    pub fn new(rate: u32, channels: usize) -> Result<Self> {
        if !(8000..=384000).contains(&rate) || !matches!(channels, 2 | 6 | 8) {
            bail!("unsupported audio rate or layout");
        }
        Ok(Self {
            queue: VecDeque::new(),
            channels,
            rate,
            phase: 0.,
        })
    }
    pub fn reset(&mut self) {
        self.queue.clear();
        self.phase = 0.;
    }
    pub fn push(&mut self, frame: &[f32]) {
        debug_assert_eq!(frame.len(), self.channels);
        self.queue.extend(
            frame
                .iter()
                .map(|v| if v.is_finite() { v.clamp(-1., 1.) } else { 0. }),
        );
        let limit = (self.rate as usize / 10).max(2) * self.channels;
        if self.queue.len() > limit {
            let excess = self.queue.len() - limit;
            self.queue.drain(..excess);
            self.phase = 0.;
        }
    }
    /// Drop the oldest queued audio back to `target_ms` once more than
    /// `limit_ms` is queued. A late tick leaves extra audio behind, and
    /// each tick sends one packet, so the delay it added stayed until the
    /// 100 ms bound; clock drift crept up to that bound as well.
    pub fn bound(&mut self, limit_ms: u32, target_ms: u32) -> bool {
        let frames = |ms: u32| (self.rate as usize * ms as usize / 1000).max(1);
        let queued = self.queue.len() / self.channels;
        if queued <= frames(limit_ms) {
            return false;
        }
        let excess = queued - frames(target_ms).min(queued);
        self.queue.drain(..excess * self.channels);
        self.phase = 0.;
        true
    }
    pub fn read(&mut self, frames: usize) -> Option<Vec<f32>> {
        if frames == 0 {
            return Some(Vec::new());
        }
        let ratio = f64::from(self.rate) / 48000.;
        let last = self.phase + (frames - 1) as f64 * ratio;
        let consumed = self.phase + frames as f64 * ratio;
        let needed = (last.floor() as usize + if last.fract() > 1e-10 { 2 } else { 1 })
            .max(consumed.floor() as usize);
        if self.queue.len() / self.channels < needed {
            return None;
        }
        let mut out = Vec::with_capacity(frames * self.channels);
        for i in 0..frames {
            let pos = self.phase + i as f64 * ratio;
            let lo = pos.floor() as usize;
            let frac = pos.fract() as f32;
            for ch in 0..self.channels {
                let a = self.queue[lo * self.channels + ch];
                let b = if frac > 1e-10 {
                    self.queue[(lo + 1) * self.channels + ch]
                } else {
                    a
                };
                out.push(a + (b - a) * frac);
            }
        }
        let whole = consumed.floor() as usize;
        self.phase = consumed - whole as f64;
        self.queue.drain(..whole * self.channels);
        Some(out)
    }
}
/// Audio the host lost before it reached the network. Windows keeps about
/// 22 ms of loopback audio (1056 frames at 48 kHz, on five endpoints of a
/// test PC including Steam Streaming Speakers) and drops what arrives while
/// that is full, without an error. The sender empties it on every pass, so
/// with 5 ms packets the backlog trim above never reaches its 30 ms: a
/// sender that runs late loses the audio inside Windows instead. A datagram
/// Winsock refuses is dropped too. Neither left a trace in the log, so a
/// stalled sender could not be told from network loss.
#[derive(Default)]
pub struct HostLoss {
    late_reads: u32,
    lost: Duration,
    longest: Duration,
    buffer: Duration,
    unsent: u64,
    reported: Option<Instant>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct HostLossReport {
    /// Reads that came after the capture buffer had filled.
    pub late_reads: u32,
    /// Audio Windows had no room for: each late wait beyond the buffer.
    pub lost: Duration,
    pub longest: Duration,
    pub buffer: Duration,
    /// Audio datagrams Winsock refused.
    pub unsent: u64,
}
impl HostLoss {
    const EVERY: Duration = Duration::from_secs(5);
    /// A read `waited` after the previous one emptied a capture buffer that
    /// holds `buffer`; a zero buffer is unknown and never counts.
    pub fn read_after(&mut self, waited: Duration, buffer: Duration) {
        if buffer.is_zero() || waited <= buffer {
            return;
        }
        self.late_reads += 1;
        self.lost += waited - buffer;
        self.longest = self.longest.max(waited);
        self.buffer = buffer;
    }
    pub fn unsent(&mut self) {
        self.unsent += 1;
    }
    /// What was lost since the last report, at most every five seconds.
    pub fn report(&mut self, now: Instant) -> Option<HostLossReport> {
        if (self.late_reads == 0 && self.unsent == 0)
            || self
                .reported
                .is_some_and(|at| now.saturating_duration_since(at) < Self::EVERY)
        {
            return None;
        }
        let report = HostLossReport {
            late_reads: self.late_reads,
            lost: self.lost,
            longest: self.longest,
            buffer: self.buffer,
            unsent: self.unsent,
        };
        *self = Self {
            reported: Some(now),
            ..Self::default()
        };
        Some(report)
    }
}
/// Windows speaker mask -> Moonlight's FL, FR, FC, LFE, BL, BR, SL, SR order.
pub fn mix_matrix(
    input_channels: usize,
    mask: u32,
    output_channels: usize,
) -> Result<Vec<Vec<f32>>> {
    if input_channels == 0 || input_channels > 32 || !matches!(output_channels, 2 | 6 | 8) {
        bail!("invalid speaker layout");
    }
    let speakers: Vec<u32> = if mask.count_ones() as usize == input_channels {
        (0..32)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| 1 << i)
            .collect()
    } else {
        let standard = [1, 2, 4, 8, 16, 32, 512, 1024];
        if input_channels == 1 {
            vec![4]
        } else {
            (0..input_channels)
                .map(|i| standard.get(i).copied().unwrap_or(0))
                .collect()
        }
    };
    let mut matrix = vec![vec![0.; input_channels]; output_channels];
    let index = |bit| speakers.iter().position(|v| *v == bit);
    if output_channels == 2 && input_channels > 2 {
        for (ch, side, rear) in [(0, 1, 16), (1, 2, 32)] {
            for (bit, gain) in [
                (side, 1.),
                (4, 0.70710677),
                (rear, 0.70710677),
                (if ch == 0 { 512 } else { 1024 }, 0.70710677),
            ] {
                if let Some(i) = index(bit) {
                    matrix[ch][i] += gain;
                }
            }
            let sum: f32 = matrix[ch].iter().sum();
            if sum > 1. {
                for v in &mut matrix[ch] {
                    *v /= sum;
                }
            }
        }
    } else if input_channels == 1 && output_channels == 2 {
        matrix[0][0] = 1.;
        matrix[1][0] = 1.;
    } else {
        for (ch, bit) in [1, 2, 4, 8, 16, 32, 512, 1024]
            .into_iter()
            .take(output_channels)
            .enumerate()
        {
            let alternate = if bit == 16 {
                512
            } else if bit == 32 {
                1024
            } else {
                bit
            };
            if let Some(i) = index(bit).or_else(|| {
                if output_channels == 6 {
                    index(alternate)
                } else {
                    None
                }
            }) {
                matrix[ch][i] = 1.;
            }
        }
    }
    Ok(matrix)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opus_layouts_preserve_legacy_surround_quality_and_validate_custom_mapping() {
        for (channels, streams, coupled, bitrate) in
            [(2, 1, 1, 512000), (6, 6, 0, 1536000), (8, 8, 0, 2048000)]
        {
            let layout = OpusLayout::select(channels, true, None).unwrap();
            assert_eq!(
                (layout.streams, layout.coupled, layout.bitrate),
                (streams, coupled, bitrate)
            );
        }
        let layout = OpusLayout::select(6, false, Some("660012345")).unwrap();
        assert_eq!((layout.streams, layout.coupled), (6, 0));
        assert_eq!(layout.mapping, [0, 1, 2, 3, 4, 5]);
        for invalid in [
            "600012345",
            "606012345",
            "642012346",
            "88001234567",
            "64201💥",
        ] {
            assert_eq!(
                OpusLayout::select(6, false, Some(invalid)).unwrap(),
                OpusLayout::select(6, false, None).unwrap()
            );
        }
    }
    #[test]
    fn fractional_rates_do_not_overrun_and_remain_bounded() {
        for rate in [8000, 44100, 48000, 96000, 192000] {
            let mut r = Resampler::new(rate, 2).unwrap();
            for i in 0..10000 {
                r.push(&[0.25, -0.25]);
                if i % 7 == 0
                    && let Some(b) = r.read(240)
                {
                    assert!(b.chunks(2).all(|f| f == [0.25, -0.25]));
                }
            }
            assert!(r.queue.len() <= rate as usize / 10 * 2);
        }
    }
    #[test]
    fn a_backlog_after_a_late_tick_is_dropped_back_to_two_packets() {
        let mut r = Resampler::new(48000, 2).unwrap();
        for i in 0..48 * 50 {
            r.push(&[i as f32 / 1e6, 0.]);
        }
        // 50 ms queued: within a 60 ms limit nothing is dropped.
        assert!(!r.bound(60, 10));
        assert!(r.bound(30, 10));
        assert_eq!(r.queue.len() / 2, 480);
        // The newest audio is kept.
        let last = r.read(480).unwrap();
        assert_eq!(last[last.len() - 2], (48 * 50 - 1) as f32 / 1e6);
    }
    #[test]
    fn host_audio_loss_counts_late_reads_and_refused_datagrams() {
        let ms = Duration::from_millis;
        let buffer = ms(22);
        let start = Instant::now();
        let mut loss = HostLoss::default();
        // On time, or with an unknown buffer: nothing to say.
        loss.read_after(ms(10), buffer);
        loss.read_after(buffer, buffer);
        loss.read_after(ms(500), Duration::ZERO);
        assert_eq!(loss.report(start), None);
        loss.read_after(ms(30), buffer);
        loss.read_after(ms(52), buffer);
        loss.unsent();
        assert_eq!(
            loss.report(start),
            Some(HostLossReport {
                late_reads: 2,
                lost: ms(38),
                longest: ms(52),
                buffer,
                unsent: 1,
            })
        );
        // Further loss waits five seconds and then reports only what is new.
        loss.unsent();
        assert_eq!(loss.report(start + ms(4999)), None);
        loss.unsent();
        assert_eq!(
            loss.report(start + ms(5000)),
            Some(HostLossReport {
                late_reads: 0,
                lost: Duration::ZERO,
                longest: Duration::ZERO,
                buffer: Duration::ZERO,
                unsent: 2,
            })
        );
        assert_eq!(loss.report(start + ms(20000)), None);
    }
    #[test]
    fn surround_upmix_does_not_duplicate_right_channel() {
        let m = mix_matrix(2, 3, 8).unwrap();
        assert_eq!(m[0], [1., 0.]);
        assert_eq!(m[1], [0., 1.]);
        assert!(m[2..].iter().flatten().all(|v| *v == 0.));
        let m = mix_matrix(6, 0x60f, 6).unwrap();
        assert_eq!(m[4][4], 1.);
        assert_eq!(m[5][5], 1.);
        assert!(
            mix_matrix(6, 0x3f, 2)
                .unwrap()
                .iter()
                .all(|r| r.iter().sum::<f32>() <= 1.00001)
        );
    }
}
