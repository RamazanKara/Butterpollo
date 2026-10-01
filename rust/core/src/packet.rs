use anyhow::{Context, Result, bail};

use crate::crypto;

/// Authenticate a message before recording its sequence. ENet's separate
/// channels can reorder messages, so accept unseen entries within a window.
#[derive(Default)]
pub struct ReplayWindow {
    newest: Option<u32>,
    seen: u64,
}
impl ReplayWindow {
    pub fn accept(&mut self, sequence: u32) -> bool {
        let Some(newest) = self.newest else {
            self.newest = Some(sequence);
            self.seen = 1;
            return true;
        };
        if sequence > newest {
            let distance = sequence - newest;
            self.seen = if distance >= 64 {
                1
            } else {
                (self.seen << distance) | 1
            };
            self.newest = Some(sequence);
            true
        } else {
            let distance = newest - sequence;
            if distance >= 64 || self.seen & (1u64 << distance) != 0 {
                return false;
            }
            self.seen |= 1u64 << distance;
            true
        }
    }
}

pub fn canonical_codec(s: &str) -> &str {
    if s.eq_ignore_ascii_case("h264") {
        "H.264"
    } else if s.eq_ignore_ascii_case("h265") || s.eq_ignore_ascii_case("hevc") {
        "HEVC"
    } else if s.eq_ignore_ascii_case("av1") {
        "AV1"
    } else if s.eq_ignore_ascii_case("pyrowave") {
        "PyroWave"
    } else {
        s
    }
}
pub fn concat_and_insert(header: usize, slice: usize, a: &[u8], b: &[u8]) -> Result<Vec<u8>> {
    if slice == 0 {
        return Ok(vec![]);
    }
    let total = a
        .len()
        .checked_add(b.len())
        .context("packet payload overflow")?;
    if total == 0 {
        return Ok(vec![]);
    }
    let count = total.div_ceil(slice);
    let len = header
        .checked_mul(count)
        .and_then(|h| h.checked_add(total))
        .context("packet buffer overflow")?;
    let mut out = Vec::new();
    out.try_reserve_exact(len)?;
    out.resize(len, 0);
    let mut source = 0;
    for i in 0..count {
        let n = slice.min(total - source);
        let dst = i * (header + slice) + header;
        let an = n.min(a.len().saturating_sub(source));
        if an > 0 {
            out[dst..dst + an].copy_from_slice(&a[source..source + an]);
        }
        if an < n {
            let start = (source + an).saturating_sub(a.len());
            out[dst + an..dst + n].copy_from_slice(&b[start..start + n - an]);
        }
        source += n;
    }
    Ok(out)
}
pub struct VideoPacketizer {
    /// Moonlight uses a 24-bit stream index alongside the 16-bit RTP sequence.
    pub sequence: u32,
    pub iv_counter: u64,
    pub frame: u32,
    pub packet_size: usize,
    pub fec_percent: usize,
    pub min_fec: usize,
    pub key: Option<[u8; 16]>,
}
pub struct PyrowaveFec {
    pub records: bool,
    pub critical_percentage: usize,
    pub detail_percentage: usize,
    pub wire_budget: usize,
    pub ipv6: bool,
}
impl VideoPacketizer {
    pub fn encode(
        &mut self,
        payload: &[u8],
        idr: bool,
        timestamp: u32,
        latency_us: u64,
    ) -> Result<Vec<Vec<u8>>> {
        self.encode_recovery(payload, idr, false, timestamp, latency_us)
    }
    pub fn encode_recovery(
        &mut self,
        payload: &[u8],
        idr: bool,
        after_invalidation: bool,
        timestamp: u32,
        latency_us: u64,
    ) -> Result<Vec<Vec<u8>>> {
        self.encode_frame(
            payload,
            idr,
            after_invalidation,
            timestamp,
            latency_us,
            None,
        )
    }
    pub fn encode_pyrowave(
        &mut self,
        payload: &[u8],
        timestamp: u32,
        latency_us: u64,
        fec: PyrowaveFec,
    ) -> Result<Vec<Vec<u8>>> {
        self.encode_frame(payload, true, false, timestamp, latency_us, Some(fec))
    }
    fn encode_frame(
        &mut self,
        payload: &[u8],
        idr: bool,
        after_invalidation: bool,
        timestamp: u32,
        latency_us: u64,
        pyrowave: Option<PyrowaveFec>,
    ) -> Result<Vec<Vec<u8>>> {
        if !(256..=1400).contains(&self.packet_size) || self.fec_percent > 100 {
            bail!("invalid packetizer parameters");
        }
        let block_size = self.packet_size + 16;
        let slice = block_size - 32;
        if payload.is_empty() || payload.len() > slice * 4092 - 8 {
            bail!("encoded frame exceeds Moonlight packet limit");
        }
        let total = payload.len() + 8;
        let total_shards = total.div_ceil(slice);
        let layout = pyrowave
            .as_ref()
            .filter(|p| p.records)
            .map(|_| {
                crate::pyrowave::layout(payload, crate::pyrowave::aligned_payload(self.packet_size))
            })
            .transpose()?;
        let critical = layout.as_ref().map_or(0, |l| l.critical_shards);
        let minimum = if pyrowave.is_some() {
            self.min_fec.max(2)
        } else {
            self.min_fec
        };
        let mut header = [0u8; 8];
        header[0] = 1;
        header[1..3].copy_from_slice(
            &((latency_us.saturating_add(50) / 100).min(65535) as u16).to_le_bytes(),
        );
        header[3] = if idr {
            2
        } else if after_invalidation {
            5
        } else {
            1
        };
        let last = total % slice;
        header[4..6]
            .copy_from_slice(&((if last == 0 { slice } else { last }) as u16).to_le_bytes());
        header[6..8].copy_from_slice(&(critical as u16).to_le_bytes());
        let data = concat_and_insert(32, slice, &header, payload)?;
        let max_data = 255 * 100 / (100 + self.fec_percent);
        let mut blocks = data.len().div_ceil(max_data * block_size);
        let mut percentage = self.fec_percent;
        if blocks > 4 {
            percentage = 0;
            blocks = 4;
        }
        let aligned = data.len().div_ceil(blocks * block_size) * block_size;
        let plan = if let Some(pyro) = pyrowave.as_ref() {
            let baseline = crate::pyrowave::plan(
                total_shards,
                critical,
                pyro.critical_percentage,
                minimum,
                0,
                0,
            )?;
            let baseline_packets = total_shards
                + baseline
                    .iter()
                    .map(|b| crate::pyrowave::parity(b.data, b.percentage, minimum))
                    .sum::<usize>();
            let wire_bytes = block_size
                + 38
                + if pyro.ipv6 { 40 } else { 20 }
                + 8
                + if self.key.is_some() { 32 } else { 0 };
            let extra = (pyro.wire_budget / wire_bytes).saturating_sub(baseline_packets);
            crate::pyrowave::plan(
                total_shards,
                critical,
                pyro.critical_percentage,
                minimum,
                pyro.detail_percentage,
                extra,
            )?
        } else {
            (0..blocks)
                .map(|block| crate::pyrowave::FecBlock {
                    data: (data.len().saturating_sub(block * aligned))
                        .min(aligned)
                        .div_ceil(block_size),
                    percentage,
                })
                .collect()
        };
        blocks = plan.len();
        let mut packets = Vec::new();
        let mut first_shard = 0;
        for (block, planned) in plan.iter().enumerate() {
            let start = first_shard * block_size;
            if start >= data.len() {
                bail!("invalid FEC block split");
            }
            let end = (start + planned.data * block_size).min(data.len());
            let source = &data[start..end];
            let count = source.len().div_ceil(block_size);
            if count >= 1024 {
                bail!("FEC shard index overflow");
            }
            let percentage = planned.percentage;
            let mut fec = (count * percentage).div_ceil(100);
            let mut effective = percentage;
            if percentage > 0 && fec < minimum {
                fec = minimum;
                effective = 100 * fec / count;
            }
            if count + fec > 255 && percentage > 0 {
                bail!("FEC parity count exceeds protocol limit");
            }
            let mut shards = vec![vec![0; block_size]; count + fec];
            for (i, s) in shards.iter_mut().take(count).enumerate() {
                let begin = i * block_size;
                let n = block_size.min(source.len() - begin);
                s[..n].copy_from_slice(&source[begin..begin + n]);
                s[16..20]
                    .copy_from_slice(&(self.sequence.wrapping_add(i as u32) << 8).to_le_bytes());
                s[20..24].copy_from_slice(&self.frame.to_le_bytes());
                s[24] = 1 | if i == 0 { 4 } else { 0 } | if i == count - 1 { 2 } else { 0 };
                s[26] = 0x10;
                if layout
                    .as_ref()
                    .is_some_and(|l| l.starts.get(first_shard + i) == Some(&true))
                {
                    s[26] |= 0x80;
                }
                s[27] = ((block as u8) << 4) | (((blocks - 1) as u8) << 6);
            }
            if fec > 0 {
                cauchy_encode(&mut shards, count, fec)?;
            }
            for (i, mut shard) in shards.into_iter().enumerate() {
                let seq = self.sequence.wrapping_add(i as u32) as u16;
                shard[0] = 0x90;
                shard[2..4].copy_from_slice(&seq.to_be_bytes());
                shard[4..8].copy_from_slice(&timestamp.to_be_bytes());
                shard[20..24].copy_from_slice(&self.frame.to_le_bytes());
                shard[27] = ((block as u8) << 4) | (((blocks - 1) as u8) << 6);
                let info = ((i as u32) << 12) | ((count as u32) << 22) | ((effective as u32) << 4);
                shard[28..32].copy_from_slice(&info.to_le_bytes());
                if let Some(k) = self.key {
                    let mut iv = [0; 12];
                    iv[..8].copy_from_slice(&self.iv_counter.to_le_bytes());
                    iv[11] = b'V';
                    self.iv_counter = self
                        .iv_counter
                        .checked_add(1)
                        .context("video nonce exhausted")?;
                    let (tag, encrypted) = crypto::gcm_seal(&k, &iv, &shard)?;
                    let mut p = Vec::with_capacity(32 + encrypted.len());
                    p.extend_from_slice(&iv);
                    p.extend_from_slice(&self.frame.to_le_bytes());
                    p.extend_from_slice(&tag);
                    p.extend_from_slice(&encrypted);
                    packets.push(p);
                } else {
                    packets.push(shard);
                }
            }
            self.sequence = self.sequence.wrapping_add((count + fec) as u32);
            first_shard += count;
        }
        self.frame = self.frame.wrapping_add(1);
        Ok(packets)
    }
}
pub fn control_header(b: &[u8]) -> Result<(u16, &[u8])> {
    if b.len() < 2 {
        bail!("short control packet");
    }
    Ok((u16::from_le_bytes(b[..2].try_into().unwrap()), &b[2..]))
}
pub fn encrypted_control(
    key: &[u8; 16],
    seq: u32,
    kind: u16,
    payload: &[u8],
    v2: bool,
) -> Result<Vec<u8>> {
    if payload.len() > u16::MAX as usize - 24 {
        bail!("control packet too large");
    }
    let mut plain = kind.to_le_bytes().to_vec();
    plain.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    plain.extend_from_slice(payload);
    let mut iv = vec![0; if v2 { 12 } else { 16 }];
    if v2 {
        iv[..4].copy_from_slice(&seq.to_le_bytes());
        iv[10] = b'H';
        iv[11] = b'C';
    } else {
        iv[0] = seq as u8;
    }
    let (tag, b) = crypto::gcm_seal(key, &iv, &plain)?;
    let mut p = 1u16.to_le_bytes().to_vec();
    p.extend_from_slice(&((4 + 16 + b.len()) as u16).to_le_bytes());
    p.extend_from_slice(&seq.to_le_bytes());
    p.extend_from_slice(&tag);
    p.extend_from_slice(&b);
    Ok(p)
}
pub fn decrypt_control(key: &[u8; 16], packet: &[u8], v2: bool) -> Result<(u32, u16, Vec<u8>)> {
    if packet.len() < 28 || u16::from_le_bytes(packet[..2].try_into().unwrap()) != 1 {
        bail!("short encrypted control packet");
    }
    let len = u16::from_le_bytes(packet[2..4].try_into().unwrap()) as usize;
    if len + 4 != packet.len() {
        bail!("control length mismatch");
    }
    let seq = u32::from_le_bytes(packet[4..8].try_into().unwrap());
    let mut iv = vec![0; if v2 { 12 } else { 16 }];
    if v2 {
        iv[..4].copy_from_slice(&seq.to_le_bytes());
        iv[10] = b'C';
        iv[11] = b'C';
    } else {
        iv[0] = seq as u8;
    }
    let b = crypto::gcm_open(key, &iv, &packet[8..24], &packet[24..])?;
    if b.len() < 4 {
        bail!("short decrypted control header");
    }
    let kind = u16::from_le_bytes(b[..2].try_into().unwrap());
    if u16::from_le_bytes(b[2..4].try_into().unwrap()) as usize != b.len() - 4 {
        bail!("decrypted control length mismatch");
    }
    Ok((seq, kind, b[4..].to_vec()))
}
const fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        let top = a & 0x80;
        a <<= 1;
        if top != 0 {
            a ^= 0x1d;
        }
        b >>= 1;
    }
    p
}
const fn inverse(a: u8) -> u8 {
    let mut p = 1;
    let mut base = a;
    let mut exponent = 254;
    while exponent != 0 {
        if exponent & 1 != 0 {
            p = gf_mul(p, base);
        }
        base = gf_mul(base, base);
        exponent >>= 1;
    }
    p
}
const fn nibble_tables() -> [[u8; 32]; 256] {
    let mut tables = [[0; 32]; 256];
    let mut coefficient = 0;
    while coefficient < 256 {
        let mut i = 0;
        while i < 16 {
            tables[coefficient][i] = gf_mul(coefficient as u8, i as u8);
            tables[coefficient][16 + i] = gf_mul(coefficient as u8, (i << 4) as u8);
            i += 1;
        }
        coefficient += 1;
    }
    tables
}
static NIBBLES: [[u8; 32]; 256] = nibble_tables();
const fn inverses() -> [u8; 256] {
    let mut values = [0; 256];
    let mut i = 0;
    while i < 256 {
        values[i] = inverse(i as u8);
        i += 1;
    }
    values
}
static INVERSES: [u8; 256] = inverses();
fn axpy(out: &mut [u8], input: &[u8], coefficient: u8) {
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        unsafe {
            axpy_avx2(out, input, &NIBBLES[coefficient as usize]);
        }
        return;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("ssse3") {
        // Both slices have the same length, checked by the caller.
        unsafe {
            axpy_ssse3(out, input, &NIBBLES[coefficient as usize]);
        }
        return;
    }
    let table = &NIBBLES[coefficient as usize];
    for (a, b) in out.iter_mut().zip(input) {
        *a ^= table[(b & 15) as usize] ^ table[16 + (b >> 4) as usize];
    }
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn axpy_avx2(out: &mut [u8], input: &[u8], table: &[u8; 32]) {
    use std::arch::x86_64::*;
    unsafe {
        let low = _mm256_broadcastsi128_si256(_mm_loadu_si128(table.as_ptr().cast()));
        let high = _mm256_broadcastsi128_si256(_mm_loadu_si128(table.as_ptr().add(16).cast()));
        let mask = _mm256_set1_epi8(15);
        let mut i = 0;
        while i + 32 <= out.len() {
            let b = _mm256_loadu_si256(input.as_ptr().add(i).cast());
            let p = _mm256_xor_si256(
                _mm256_shuffle_epi8(low, _mm256_and_si256(b, mask)),
                _mm256_shuffle_epi8(high, _mm256_and_si256(_mm256_srli_epi16::<4>(b), mask)),
            );
            let a = _mm256_loadu_si256(out.as_ptr().add(i).cast());
            _mm256_storeu_si256(out.as_mut_ptr().add(i).cast(), _mm256_xor_si256(a, p));
            i += 32;
        }
        for (a, b) in out[i..].iter_mut().zip(&input[i..]) {
            *a ^= table[(b & 15) as usize] ^ table[16 + (b >> 4) as usize];
        }
    }
}

// Reuse each input load and nibble split across four parity rows. A row is only
// 1–2 KiB for Moonlight; the active parity set stays in L1 instead of rereading
// the full input block independently for every row.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn parity_avx2<const ROWS: usize>(
    source: &[Vec<u8>],
    destination: &mut [Vec<u8>],
    first: usize,
    parity: usize,
) {
    use std::arch::x86_64::*;
    unsafe {
        let size = source[0].len();
        let pointers: [*mut u8; ROWS] = std::array::from_fn(|i| destination[i].as_mut_ptr());
        let mask = _mm256_set1_epi8(15);
        for (column, input) in source.iter().enumerate() {
            let coefficients: [u8; ROWS] =
                std::array::from_fn(|row| INVERSES[(parity + column) ^ (first + row)]);
            let low: [__m256i; ROWS] = std::array::from_fn(|row| {
                _mm256_broadcastsi128_si256(_mm_loadu_si128(
                    NIBBLES[coefficients[row] as usize].as_ptr().cast(),
                ))
            });
            let high: [__m256i; ROWS] = std::array::from_fn(|row| {
                _mm256_broadcastsi128_si256(_mm_loadu_si128(
                    NIBBLES[coefficients[row] as usize].as_ptr().add(16).cast(),
                ))
            });
            let mut position = 0;
            while position + 32 <= size {
                let value = _mm256_loadu_si256(input.as_ptr().add(position).cast());
                let lo = _mm256_and_si256(value, mask);
                let hi = _mm256_and_si256(_mm256_srli_epi16::<4>(value), mask);
                for row in 0..ROWS {
                    let product = _mm256_xor_si256(
                        _mm256_shuffle_epi8(low[row], lo),
                        _mm256_shuffle_epi8(high[row], hi),
                    );
                    let output = _mm256_loadu_si256(pointers[row].add(position).cast());
                    _mm256_storeu_si256(
                        pointers[row].add(position).cast(),
                        _mm256_xor_si256(output, product),
                    );
                }
                position += 32;
            }
            for (position, value) in input.iter().enumerate().skip(position) {
                for row in 0..ROWS {
                    let table = &NIBBLES[coefficients[row] as usize];
                    *pointers[row].add(position) ^=
                        table[(value & 15) as usize] ^ table[16 + (value >> 4) as usize];
                }
            }
        }
    }
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn axpy_ssse3(out: &mut [u8], input: &[u8], table: &[u8; 32]) {
    use std::arch::x86_64::*;
    unsafe {
        let low = _mm_loadu_si128(table.as_ptr().cast());
        let high = _mm_loadu_si128(table.as_ptr().add(16).cast());
        let mask = _mm_set1_epi8(15);
        let mut i = 0;
        while i + 16 <= out.len() {
            let b = _mm_loadu_si128(input.as_ptr().add(i).cast());
            let p = _mm_xor_si128(
                _mm_shuffle_epi8(low, _mm_and_si128(b, mask)),
                _mm_shuffle_epi8(high, _mm_and_si128(_mm_srli_epi16::<4>(b), mask)),
            );
            let a = _mm_loadu_si128(out.as_ptr().add(i).cast());
            _mm_storeu_si128(out.as_mut_ptr().add(i).cast(), _mm_xor_si128(a, p));
            i += 16;
        }
        for (a, b) in out[i..].iter_mut().zip(&input[i..]) {
            *a ^= table[(b & 15) as usize] ^ table[16 + (b >> 4) as usize];
        }
    }
}

/// NVIDIA's separate input cipher chains the next IV from the last 16 bytes
/// of each authenticated message. Never advance it on a forged packet.
pub struct LegacyInput {
    iv: [u8; 16],
}
impl LegacyInput {
    pub fn new(key_id: u32) -> Self {
        let mut iv = [0; 16];
        iv[..4].copy_from_slice(&key_id.to_be_bytes());
        Self { iv }
    }
    pub fn open(&mut self, key: &[u8; 16], payload: &[u8]) -> Result<Vec<u8>> {
        if payload.len() < 20 {
            bail!("short legacy input message");
        }
        let len = u32::from_be_bytes(payload[..4].try_into().unwrap()) as usize;
        if len < 16 || len + 4 != payload.len() {
            bail!("legacy input length mismatch");
        }
        let tagged = &payload[4..];
        let plain = crypto::gcm_open(key, &self.iv, &tagged[..16], &tagged[16..])?;
        if len >= 32 {
            self.iv.copy_from_slice(&tagged[len - 16..]);
        }
        Ok(plain)
    }
}
pub struct AudioPacketizer {
    pub sequence: u16,
    pub timestamp: u32,
    pub key: [u8; 16],
    pub key_id: u32,
    pub encrypted: bool,
    pub packet_ms: u32,
    pending: Vec<Vec<u8>>,
}
impl AudioPacketizer {
    pub fn new(key: [u8; 16], key_id: u32, encrypted: bool, packet_ms: u32) -> Self {
        Self {
            sequence: 0,
            timestamp: 0,
            key,
            key_id,
            encrypted,
            packet_ms,
            pending: vec![],
        }
    }
    pub fn encode(&mut self, opus: &[u8]) -> Result<Vec<Vec<u8>>> {
        let seq = self.sequence;
        let mut iv = [0; 16];
        iv[..4].copy_from_slice(&self.key_id.wrapping_add(u32::from(seq)).to_be_bytes());
        let data = if self.encrypted {
            crypto::cbc_seal(&self.key, &iv, opus)
        } else {
            opus.to_vec()
        };
        let mut p = vec![0; 12];
        p[0] = 0x80;
        p[1] = 97;
        p[2..4].copy_from_slice(&seq.to_be_bytes());
        p[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        p.extend_from_slice(&data);
        let mut output = vec![p];
        self.pending.push(data);
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(self.packet_ms);
        if self.pending.len() == 4 {
            let len = self.pending.iter().map(Vec::len).max().unwrap();
            let matrix = [[0x77, 0x40, 0x38, 0x0e], [0xc7, 0xa7, 0x0d, 0x6c]];
            for (index, row) in matrix.iter().enumerate() {
                let mut p = vec![0; 24 + len];
                p[0] = 0x80;
                p[1] = 127;
                p[2..4].copy_from_slice(&self.sequence.wrapping_add(index as u16).to_be_bytes());
                p[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
                p[12] = index as u8;
                p[13] = 97;
                p[14..16].copy_from_slice(&seq.wrapping_sub(3).to_be_bytes());
                p[16..20].copy_from_slice(
                    &self
                        .timestamp
                        .wrapping_sub(4 * self.packet_ms)
                        .to_be_bytes(),
                );
                for (j, b) in self.pending.iter().enumerate() {
                    axpy(&mut p[24..24 + b.len()], b, row[j]);
                }
                output.push(p);
            }
            self.pending.clear();
        }
        Ok(output)
    }
}

/// nanors uses a Cauchy matrix; the Vandermonde matrix used by many RS crates is not wire compatible.
pub fn cauchy_encode(shards: &mut [Vec<u8>], data: usize, parity: usize) -> Result<()> {
    if data == 0 || data + parity != shards.len() || data + parity > 255 {
        bail!("invalid Cauchy shard count");
    }
    let size = shards[0].len();
    if shards.iter().any(|s| s.len() != size) {
        bail!("unequal shard sizes");
    }
    let (source, dest) = shards.split_at_mut(data);
    for out in dest.iter_mut() {
        out.fill(0);
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        let (rows, remainder) = dest.as_chunks_mut::<4>();
        for (batch, out) in rows.iter_mut().enumerate() {
            // Validation above guarantees disjoint, equally sized buffers and
            // at most 255 rows/columns. Each group writes only its own rows.
            unsafe {
                parity_avx2::<4>(source, out, batch * 4, parity);
            }
        }
        let first = parity - remainder.len();
        unsafe {
            match remainder.len() {
                1 => parity_avx2::<1>(source, remainder, first, parity),
                2 => parity_avx2::<2>(source, remainder, first, parity),
                3 => parity_avx2::<3>(source, remainder, first, parity),
                _ => (),
            }
        }
        return Ok(());
    }
    for (row, out) in dest.iter_mut().enumerate() {
        for (i, input) in source.iter().enumerate() {
            let base = (parity + i) as u8 ^ row as u8;
            axpy(out, input, INVERSES[base as usize]);
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_window_accepts_reordering_but_rejects_duplicates_and_wraps() {
        let mut window = ReplayWindow::default();
        for n in [100, 102, 101, 103, 99] {
            assert!(window.accept(n));
        }
        for n in [100, 99, 102, 1] {
            assert!(!window.accept(n));
        }
        assert!(window.accept(u32::MAX));
        assert!(!window.accept(0));
    }
    #[test]
    fn forged_legacy_input_cannot_change_the_next_iv() {
        let key = [3; 16];
        let mut input = LegacyInput::new(0x12345678);
        let original = input.iv;
        let raw = vec![7; 40];
        let (tag, encrypted) = crypto::gcm_seal(&key, &original, &raw).unwrap();
        let mut packet = ((tag.len() + encrypted.len()) as u32)
            .to_be_bytes()
            .to_vec();
        packet.extend_from_slice(&tag);
        packet.extend_from_slice(&encrypted);
        let mut bad = packet.clone();
        bad[5] ^= 1;
        assert!(input.open(&key, &bad).is_err());
        assert_eq!(input.iv, original);
        assert_eq!(input.open(&key, &packet).unwrap(), raw);
        assert_eq!(input.iv, packet[packet.len() - 16..]);
    }
    #[test]
    fn simd_cauchy_matches_scalar_for_every_coefficient_and_tail() {
        for coefficient in 0..=255 {
            for len in [1, 15, 16, 17, 31, 33, 1023] {
                let source: Vec<u8> = (0..len).map(|i| (i * 71 + 13) as u8).collect();
                let mut out = vec![0x55; len];
                axpy(&mut out, &source, coefficient);
                let expected: Vec<u8> = source
                    .iter()
                    .map(|v| 0x55 ^ gf_mul(*v, coefficient))
                    .collect();
                assert_eq!(out, expected);
            }
        }
    }
    #[test]
    fn batched_cauchy_matches_independent_scalar_for_row_groups_and_short_tails() {
        for (data, parity) in [
            (1, 254),
            (254, 1),
            (4, 2),
            (4, 3),
            (4, 4),
            (4, 5),
            (4, 6),
            (4, 7),
            (32, 7),
            (96, 20),
            (192, 39),
        ] {
            for length in [0, 1, 15, 31, 32, 33, 63, 65, 1416] {
                let mut shards: Vec<Vec<u8>> = (0..data + parity)
                    .map(|i| (0..length).map(|j| (i * 29 + j * 71 + 13) as u8).collect())
                    .collect();
                let mut expected = shards.clone();
                for (row, out) in expected[data..].iter_mut().enumerate() {
                    out.fill(0);
                    for (column, input) in shards[..data].iter().enumerate() {
                        let coefficient = inverse((parity + column) as u8 ^ row as u8);
                        for (out, input) in out.iter_mut().zip(input) {
                            *out ^= gf_mul(*input, coefficient);
                        }
                    }
                }
                cauchy_encode(&mut shards, data, parity).unwrap();
                assert_eq!(shards, expected, "matrix {data}+{parity}, {length} bytes");
            }
        }
    }
    #[test]
    fn packet_insert_crosses_sources_at_every_boundary() {
        for h in 0..10 {
            for s in 0..16 {
                for a in 0..25 {
                    for b in 0..25 {
                        let aa = vec![1; a];
                        let bb = vec![2; b];
                        let joined = [aa.clone(), bb.clone()].concat();
                        let expected: Vec<u8> = if s == 0 {
                            vec![]
                        } else {
                            joined
                                .chunks(s)
                                .flat_map(|p| [vec![0; h], p.to_vec()].concat())
                                .collect()
                        };
                        assert_eq!(concat_and_insert(h, s, &aa, &bb).unwrap(), expected);
                    }
                }
            }
        }
    }
    #[test]
    fn video_headers_follow_moonlight_layout() {
        let mut p = VideoPacketizer {
            sequence: 65535,
            iv_counter: 0,
            frame: 1,
            packet_size: 1024,
            fec_percent: 0,
            min_fec: 0,
            key: None,
        };
        let packets = p.encode(&vec![3; 1100], true, 900, 200).unwrap();
        assert_eq!(packets.len(), 2);
        assert_eq!(&packets[0][..4], &[0x90, 0, 0xff, 0xff]);
        assert_eq!(packets[0][24], 5);
        assert_eq!(packets[1][24], 3);
        assert_eq!(&packets[0][32..40], &[1, 2, 0, 2, 100, 0, 0, 0]);
        assert_eq!(&packets[1][2..4], &[0, 0]);
        let stream_index =
            |packet: &[u8]| u32::from_le_bytes(packet[16..20].try_into().unwrap()) >> 8;
        assert_eq!(stream_index(&packets[0]), 65535);
        assert_eq!(stream_index(&packets[1]), 65536);
        let recovered = p.encode_recovery(&[3; 16], false, true, 1000, 300).unwrap();
        assert_eq!(recovered[0][35], 5); // Moonlight recovery marker
        assert_eq!(stream_index(&recovered[0]), 65537);
        let keyframe = p.encode_recovery(&[3; 16], true, true, 1100, 300).unwrap();
        assert_eq!(keyframe[0][35], 2); // IDR takes precedence
        p.sequence = 0xFFFFFF;
        let wrapped = p.encode(&vec![3; 1100], true, 1200, 300).unwrap();
        assert_eq!(stream_index(&wrapped[0]), 0xFFFFFF);
        assert_eq!(stream_index(&wrapped[1]), 0);
        assert_eq!(&wrapped[0][2..4], &[0xff, 0xff]);
        assert_eq!(&wrapped[1][2..4], &[0, 0]);
    }
    #[test]
    fn encrypted_video_nonce_is_unique_and_authenticated() {
        let k = [5; 16];
        let mut p = VideoPacketizer {
            sequence: 0,
            iv_counter: 0,
            frame: 1,
            packet_size: 1024,
            fec_percent: 20,
            min_fec: 1,
            key: Some(k),
        };
        let b = p.encode(&vec![1; 1100], false, 123, 0).unwrap();
        assert_ne!(&b[0][..12], &b[1][..12]);
        let plain = crypto::gcm_open(&k, &b[0][..12], &b[0][16..32], &b[0][32..]).unwrap();
        assert_eq!(plain[0], 0x90);
        assert_eq!(plain[24], 5);
    }
}
