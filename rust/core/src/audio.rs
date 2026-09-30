//! Bounded audio buffering, speaker mapping and fractional resampling.
use anyhow::{Result, bail};
use std::collections::VecDeque;

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
