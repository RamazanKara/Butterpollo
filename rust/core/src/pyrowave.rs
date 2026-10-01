//! Vibepollo 2.0 PyroWave framing, partial recovery and transport budgets.
use anyhow::{Context, Result, bail};
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};
pub const BITSTREAM_ID: &str = "186f0393";
pub const PACKET_BOUNDARY: usize = 1024;
const PADDING: u32 = u32::MAX;
fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
pub fn aligned_payload(packet_size: usize) -> usize {
    let size = packet_size.saturating_sub(16);
    if size >= 24 && size.is_multiple_of(4) {
        size
    } else {
        0
    }
}
pub fn max_frame_bytes(packet_size: usize, critical_fec: bool) -> usize {
    (packet_size.saturating_sub(16) * if critical_fec { 3000 } else { 4000 }).saturating_sub(8)
}
pub fn budget(
    kbps: u32,
    interval: Duration,
    packet_size: usize,
    records: bool,
    critical_fec: bool,
) -> usize {
    let limit = max_frame_bytes(packet_size, critical_fec);
    let limit = if records {
        limit * 95 / 100
    } else {
        // The codec can emit eight-byte blocks. Each then needs a four-byte
        // length, so reserve the worst case rather than assuming full packets.
        limit.saturating_sub(4) / 12 * 8
    };
    ((f64::from(kbps) * 125. * interval.as_secs_f64()) as usize).min(limit) & !3
}
pub fn container(bitstream: &[u8], packets: &[(usize, usize)]) -> Result<Vec<u8>> {
    if packets.is_empty() || packets.len() > u32::MAX as usize {
        bail!("invalid PyroWave packet count");
    }
    let mut out = Vec::with_capacity(bitstream.len() + 4 + packets.len() * 4);
    out.extend_from_slice(&(packets.len() as u32).to_le_bytes());
    for &(offset, size) in packets {
        let end = offset
            .checked_add(size)
            .context("PyroWave packet overflow")?;
        if size == 0 || size > u32::MAX as usize || end > bitstream.len() {
            bail!("invalid PyroWave packet slice");
        }
        out.extend_from_slice(&(size as u32).to_le_bytes());
        out.extend_from_slice(&bitstream[offset..end]);
    }
    Ok(out)
}
#[derive(Clone, Copy)]
struct Record {
    offset: usize,
    size: usize,
    index: u32,
}
fn records(frame: &[u8]) -> Result<Vec<Record>> {
    if frame.len() < 8
        || !frame.len().is_multiple_of(4)
        || word(frame, 0) & 0x80000000 == 0
        || word(frame, 0) == PADDING
        || (word(frame, 4) >> 24) & 3 != 0
    {
        bail!("invalid PyroWave sequence header");
    }
    let sequence = (word(frame, 0) >> 28) & 7;
    let mut records = Vec::new();
    let mut offset = 8;
    while offset < frame.len() {
        if frame.len() - offset < 8 {
            bail!("short PyroWave record");
        }
        let header = word(frame, offset);
        let padding = header == PADDING;
        let size = if padding {
            8usize
                .checked_add(word(frame, offset + 4) as usize * 4)
                .context("padding overflow")?
        } else {
            (((header >> 16) & 0xfff) as usize) * 4
        };
        if size < 8
            || size > frame.len() - offset
            || (!padding && (header & 0x80000000 != 0 || (header >> 28) & 7 != sequence))
        {
            bail!("invalid PyroWave block record");
        }
        if !padding {
            records.push(Record {
                offset,
                size,
                index: word(frame, offset + 4) >> 8,
            });
        }
        offset += size;
    }
    if records.len() != (word(frame, 4) & 0xffffff) as usize {
        bail!("PyroWave record count mismatch");
    }
    Ok(records)
}
fn coarse_count(frame: &[u8]) -> u32 {
    let extent = |n: u32| n.div_ceil(32).max(4).div_ceil(32);
    let header = word(frame, 0);
    extent((header & 0x3fff) + 1) * extent(((header >> 14) & 0x3fff) + 1) * 12
}
fn padding(out: &mut Vec<u8>, bytes: usize) {
    debug_assert!(bytes >= 8 && bytes.is_multiple_of(4));
    out.extend_from_slice(&PADDING.to_le_bytes());
    out.extend_from_slice(&((bytes - 8) as u32 / 4).to_le_bytes());
    out.resize(out.len() + bytes - 8, 0);
}
// Earliest fitting record in O(log 4096), preserving encoder order for ties.
struct Fits {
    queues: Vec<VecDeque<usize>>,
    tree: Vec<usize>,
}
impl Fits {
    fn new(records: &[Record]) -> Self {
        let mut s = Self {
            queues: vec![VecDeque::new(); 4096],
            tree: vec![usize::MAX; 8192],
        };
        for (i, r) in records.iter().enumerate() {
            s.queues[r.size / 4].push_back(i);
        }
        for i in 0..4096 {
            s.tree[4096 + i] = s.queues[i].front().copied().unwrap_or(usize::MAX);
        }
        for i in (1..4096).rev() {
            s.tree[i] = s.tree[2 * i].min(s.tree[2 * i + 1]);
        }
        s
    }
    fn fitting(&self, words: usize) -> usize {
        let (mut lo, mut hi, mut result) = (4096, 4096 + words.min(4095) + 1, usize::MAX);
        while lo < hi {
            if lo & 1 != 0 {
                result = result.min(self.tree[lo]);
                lo += 1;
            }
            if hi & 1 != 0 {
                hi -= 1;
                result = result.min(self.tree[hi]);
            }
            lo /= 2;
            hi /= 2;
        }
        result
    }
    fn exact(&self, words: usize) -> usize {
        self.tree.get(4096 + words).copied().unwrap_or(usize::MAX)
    }
    fn remove(&mut self, words: usize) {
        self.queues[words].pop_front();
        let mut at = 4096 + words;
        self.tree[at] = self.queues[words].front().copied().unwrap_or(usize::MAX);
        while at > 1 {
            at /= 2;
            self.tree[at] = self.tree[2 * at].min(self.tree[2 * at + 1]);
        }
    }
}
pub fn record_frame(bitstream: &[u8], payload: usize, limit: usize) -> Result<Vec<u8>> {
    let all = records(bitstream)?;
    if bitstream.len() > limit {
        bail!("PyroWave frame exceeds transport capacity");
    }
    if payload == 0 {
        return Ok(bitstream.to_vec());
    }
    let coarse = coarse_count(bitstream);
    let mut out = Vec::with_capacity(bitstream.len());
    out.extend_from_slice(&bitstream[..8]);
    for required in [true, false] {
        let mut ordinary = Vec::new();
        for r in all.iter().filter(|r| (r.index < coarse) == required) {
            if r.size > payload - 8 {
                let room = payload - (out.len() + 8) % payload;
                if r.size % payload == room - 4 {
                    padding(&mut out, 8);
                }
                out.extend_from_slice(&bitstream[r.offset..r.offset + r.size]);
            } else {
                ordinary.push(*r);
            }
        }
        let mut fits = Fits::new(&ordinary);
        for _ in 0..ordinary.len() {
            loop {
                let room = payload - (out.len() + 8) % payload;
                let mut chosen = fits.fitting(room / 4);
                if chosen != usize::MAX && room - ordinary[chosen].size == 4 {
                    chosen = fits.exact(room / 4).min(if room >= 16 {
                        fits.fitting((room - 8) / 4)
                    } else {
                        usize::MAX
                    });
                }
                if chosen == usize::MAX {
                    padding(&mut out, room);
                    continue;
                }
                let r = ordinary[chosen];
                fits.remove(r.size / 4);
                out.extend_from_slice(&bitstream[r.offset..r.offset + r.size]);
                break;
            }
        }
    }
    if out.len() > limit {
        return Ok(bitstream.to_vec());
    }
    Ok(out)
}
pub struct Layout {
    pub critical_shards: usize,
    pub starts: Vec<bool>,
}
pub fn layout(frame: &[u8], payload: usize) -> Result<Layout> {
    let all = records(frame)?;
    let mut layout = Layout {
        critical_shards: 0,
        starts: vec![false; (frame.len() + 8).div_ceil(payload.max(1))],
    };
    if payload == 0 {
        return Ok(layout);
    }
    let coarse = coarse_count(frame);
    let mut fine_seen = false;
    let mut critical = 8;
    for r in &all {
        if r.index < coarse {
            if fine_seen {
                return Ok(layout);
            }
            critical = r.offset + r.size;
        } else {
            fine_seen = true;
        }
        if r.size <= payload - 8 && (r.offset + 8) % payload + r.size > payload {
            return Ok(layout);
        }
    }
    layout.critical_shards = (critical + 8).div_ceil(payload);
    let mut offset = 0;
    while offset < frame.len() {
        if offset == 0 || (offset + 8).is_multiple_of(payload) {
            layout.starts[(offset + 8) / payload] = true;
        }
        let header = word(frame, offset);
        offset += if header == PADDING {
            8 + word(frame, offset + 4) as usize * 4
        } else if header & 0x80000000 != 0 {
            8
        } else {
            ((header >> 16) & 0xfff) as usize * 4
        };
    }
    Ok(layout)
}
#[derive(Debug, Clone, Copy)]
pub struct FecBlock {
    pub data: usize,
    pub percentage: usize,
}
pub fn parity(data: usize, percentage: usize, minimum: usize) -> usize {
    if percentage == 0 {
        0
    } else {
        (data * percentage).div_ceil(100).max(minimum)
    }
}
fn split(blocks: &mut Vec<FecBlock>, data: usize, slots: usize) {
    if data == 0 {
        return;
    }
    let count = data.div_ceil(255).clamp(1, slots);
    let size = data.div_ceil(count);
    for i in 0..count {
        blocks.push(FecBlock {
            data: if i + 1 == count {
                data - size * i
            } else {
                size
            },
            percentage: 0,
        });
    }
}
pub fn plan(
    total: usize,
    critical: usize,
    rate: usize,
    minimum: usize,
    detail: usize,
    extra: usize,
) -> Result<Vec<FecBlock>> {
    if total == 0 || total > 4000 || rate > 255 || minimum > 255 {
        bail!("invalid PyroWave FEC plan");
    }
    let critical = critical.min(total);
    let mut out = vec![];
    if critical == 0 || rate == 0 || critical + parity(critical, rate, minimum) > 255 {
        split(&mut out, total, 4);
    } else {
        out.push(FecBlock {
            data: critical,
            percentage: rate,
        });
        let fine = total - critical;
        let detail = detail.min(50);
        let mut extra = extra.min(parity(fine, detail, 0));
        let capacity = |rate| {
            (1..=255)
                .rev()
                .find(|&data| data + parity(data, rate, minimum) <= 255)
                .unwrap_or(0)
        };
        for percentage in (1..=detail).rev() {
            let cap = capacity(percentage);
            if cap == 0 || fine == 0 {
                continue;
            }
            let count = fine.div_ceil(cap);
            if count > 3 {
                continue;
            }
            let candidate: Vec<_> = (0..count)
                .map(|i| FecBlock {
                    data: fine / count + usize::from(i < fine % count),
                    percentage,
                })
                .collect();
            if candidate
                .iter()
                .map(|b| parity(b.data, b.percentage, minimum))
                .sum::<usize>()
                <= extra
            {
                out.extend(candidate);
                return Ok(out);
            }
        }
        let base = parity(critical, rate, minimum);
        if detail > 0 && extra > 0 {
            let mut leading = critical;
            while leading < total.min(capacity(rate))
                && parity(leading + 1, rate, minimum) - base <= extra
            {
                leading += 1;
            }
            out[0].data = leading;
            extra -= parity(leading, rate, minimum) - base;
            let mut remaining = total - leading;
            while remaining > 0 && out.len() < 4 {
                let mut data = remaining.min(capacity(detail));
                while data > 0 && parity(data, detail, minimum) > extra {
                    data -= 1;
                }
                if data == 0 || remaining - data > (3 - out.len()) * 1023 {
                    break;
                }
                out.push(FecBlock {
                    data,
                    percentage: detail,
                });
                extra -= parity(data, detail, minimum);
                remaining -= data;
            }
            if remaining > 0 {
                let slots = 4 - out.len();
                split(&mut out, remaining, slots);
            }
        } else {
            split(&mut out, fine, 3);
        }
    }
    if out.len() > 4
        || out.iter().any(|b| {
            b.data > 1023
                || (b.percentage > 0 && b.data + parity(b.data, b.percentage, minimum) > 255)
        })
    {
        bail!("PyroWave frame exceeds FEC capacity");
    }
    Ok(out)
}
/// No idle credit: detail protection is bounded by this frame's allowance.
pub struct DetailFec {
    nominal: f64,
    previous: Option<Instant>,
    interval: f64,
    stable: f64,
    blocks: BTreeMap<u32, (u64, usize)>,
}
impl DetailFec {
    pub fn new(fps_millihz: u32) -> Self {
        Self {
            nominal: 1000. / f64::from(fps_millihz.max(1)),
            previous: None,
            interval: 0.,
            stable: 0.,
            blocks: BTreeMap::new(),
        }
    }
    pub fn observe(&mut self, frame: &[u8], now: Instant, kbps: u32) -> Result<(usize, usize)> {
        let elapsed = self
            .previous
            .replace(now)
            .map(|p| now.saturating_duration_since(p).as_secs_f64())
            .unwrap_or(0.);
        if elapsed <= self.nominal * 1.01 {
            self.blocks.clear();
            self.stable = 0.;
            self.interval = elapsed;
            return Ok((0, 0));
        }
        let mut next = BTreeMap::new();
        let (mut total, mut unchanged) = (0, 0);
        for r in records(frame)? {
            let hash = frame[r.offset..r.offset + r.size].iter().enumerate().fold(
                0xcbf29ce484222325u64,
                |h, (i, &b)| {
                    (h ^ u64::from(if i == 3 { b & 0x8f } else { b })).wrapping_mul(0x100000001b3)
                },
            );
            if self.blocks.get(&r.index) == Some(&(hash, r.size)) {
                unchanged += r.size;
            }
            total += r.size;
            next.insert(r.index, (hash, r.size));
        }
        self.blocks = next;
        self.interval = if self.interval > 0. {
            self.interval * 0.8 + elapsed * 0.2
        } else {
            elapsed
        };
        if total == 0 || unchanged * 4 < total * 3 {
            self.stable = 0.;
            return Ok((0, 0));
        }
        self.stable += elapsed;
        if self.stable < 0.25 {
            return Ok((0, 0));
        }
        Ok((
            (100. * (self.interval / self.nominal - 1.)).clamp(0., 50.) as usize,
            (f64::from(kbps) * 125. * elapsed.min(self.interval).min(0.05)) as usize,
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn frame(records: &[(u32, usize)]) -> Vec<u8> {
        let mut out = vec![];
        out.extend_from_slice(&(0x80000000u32 | 1919 | (1079 << 14)).to_le_bytes());
        out.extend_from_slice(&(records.len() as u32).to_le_bytes());
        for &(index, size) in records {
            out.extend_from_slice(&((size as u32 / 4) << 16).to_le_bytes());
            out.extend_from_slice(&(index << 8).to_le_bytes());
            out.resize(out.len() + size - 8, 42);
        }
        out
    }
    #[test]
    fn compatibility_matches_clients_and_checks_slices() {
        assert_eq!(
            container(&[10, 11, 12, 13], &[(1, 2), (0, 1)]).unwrap(),
            [2, 0, 0, 0, 2, 0, 0, 0, 11, 12, 1, 0, 0, 0, 10]
        );
        assert!(container(&[1], &[(usize::MAX, 1)]).is_err());
        let cap = max_frame_bytes(1392, false);
        let raw = budget(2_000_000, Duration::from_secs(1), 1392, false, false);
        assert!(4 + raw + (raw / 8) * 4 <= cap);
        assert_eq!(
            budget(800000, Duration::from_secs_f64(1. / 120.), 1392, true, true),
            833332
        );
    }
    #[test]
    fn aligned_records_keep_coarse_first_and_restart_after_loss() {
        let original = frame(&[(24, 40), (0, 16), (1, 60), (27, 100), (26, 12)]);
        let packed = record_frame(&original, 64, 10000).unwrap();
        assert_eq!(records(&packed).unwrap().len(), 5);
        let l = layout(&packed, 64).unwrap();
        assert!(l.critical_shards >= 2);
        assert!(l.starts[0]);
        assert_eq!(aligned_payload(1390), 0);
        assert!(record_frame(&[0; 8], 64, 10000).is_err());
        let mut bad = original;
        bad[8..12].copy_from_slice(&0x10000u32.to_le_bytes());
        assert!(record_frame(&bad, 64, 10000).is_err());
    }
    #[test]
    fn critical_and_adaptive_parity_obey_wire_and_block_limits() {
        let maximum = plan(4000, 0, 20, 2, 0, 0).unwrap();
        assert_eq!(maximum.len(), 4);
        assert_eq!(maximum.iter().map(|b| b.data).sum::<usize>(), 4000);
        assert!(maximum.iter().all(|b| b.data <= 1023 && b.percentage == 0));
        for total in [1, 100, 800, 2000, 3000] {
            let p = plan(total, 2, 20, 2, 50, 100).unwrap();
            assert_eq!(p.iter().map(|b| b.data).sum::<usize>(), total);
            assert!(p.len() <= 4);
            assert!(
                p.iter()
                    .map(|b| parity(b.data, b.percentage, 2))
                    .sum::<usize>()
                    <= 102
            );
        }
        assert_eq!(
            plan(1000, 0, 20, 2, 0, 0)
                .unwrap()
                .iter()
                .map(|b| parity(b.data, b.percentage, 2))
                .sum::<usize>(),
            0
        );
    }
    #[test]
    fn low_cadence_static_detail_gets_parity_and_motion_disables_it() {
        let mut controller = DetailFec::new(120000);
        let start = Instant::now();
        let same = frame(&[(0, 16), (30, 20)]);
        let mut decision = (0, 0);
        for i in 0..20 {
            decision = controller
                .observe(&same, start + Duration::from_millis(i * 20), 800000)
                .unwrap();
        }
        assert_eq!(decision.0, 50);
        assert!(decision.1 <= 2_000_000);
        assert_eq!(
            controller
                .observe(
                    &frame(&[(0, 20), (30, 24)]),
                    start + Duration::from_millis(400),
                    800000
                )
                .unwrap(),
            (0, 0)
        );
    }
}
