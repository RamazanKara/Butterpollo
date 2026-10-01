//! Minimal bounded SPS edit for H.264 long-term reference packet-loss recovery.
struct Bits<'a> {
    bytes: &'a mut [u8],
    at: usize,
}
impl Bits<'_> {
    fn read(&mut self, count: usize) -> Option<u32> {
        if count > 32 || self.at.checked_add(count)? > self.bytes.len() * 8 {
            return None;
        }
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | u32::from((self.bytes[self.at / 8] >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Some(value)
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeroes = 0;
        while self.read(1)? == 0 {
            zeroes += 1;
            if zeroes > 30 {
                return None;
            }
        }
        Some(((1u32 << zeroes) - 1) + self.read(zeroes)?)
    }
    fn se(&mut self) -> Option<i32> {
        let value = self.ue()?;
        Some(if value & 1 != 0 {
            (value.div_ceil(2)) as i32
        } else {
            -((value / 2) as i32)
        })
    }
}
fn allow_gaps(sps: &[u8]) -> Option<Vec<u8>> {
    if sps.len() > 4096 || sps.first()? & 31 != 7 {
        return None;
    }
    let mut rbsp = vec![];
    let mut zeros = 0;
    for &byte in &sps[1..] {
        if byte == 3 && zeros == 2 {
            zeros = 0;
            continue;
        }
        rbsp.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    let mut bits = Bits {
        bytes: &mut rbsp,
        at: 0,
    };
    let profile = bits.read(8)?;
    bits.read(16)?;
    bits.ue()?;
    if [
        100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135, 144,
    ]
    .contains(&profile)
    {
        let chroma = bits.ue()?;
        if chroma > 3 {
            return None;
        }
        if chroma == 3 {
            bits.read(1)?;
        }
        bits.ue()?;
        bits.ue()?;
        bits.read(1)?;
        if bits.read(1)? != 0 {
            for index in 0..if chroma == 3 { 12 } else { 8 } {
                if bits.read(1)? == 0 {
                    continue;
                }
                let (mut last, mut next) = (8i32, 8i32);
                for _ in 0..if index < 6 { 16 } else { 64 } {
                    if next != 0 {
                        next = (last + bits.se()?).rem_euclid(256);
                    }
                    if next != 0 {
                        last = next;
                    }
                }
            }
        }
    } else if ![66, 77, 88].contains(&profile) {
        return None;
    }
    bits.ue()?;
    match bits.ue()? {
        0 => {
            bits.ue()?;
        }
        1 => {
            bits.read(1)?;
            bits.se()?;
            bits.se()?;
            let count = bits.ue()?;
            if count > 256 {
                return None;
            }
            for _ in 0..count {
                bits.se()?;
            }
        }
        2 => {}
        _ => return None,
    }
    bits.ue()?;
    let gap = bits.at;
    bits.read(1)?;
    bits.bytes[gap / 8] |= 1 << (7 - gap % 8);
    let mut output = vec![sps[0]];
    let mut zeros = 0;
    for byte in rbsp {
        if zeros == 2 && byte <= 3 {
            output.push(3);
            zeros = 0;
        }
        output.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    Some(output)
}
/// Preserve every NAL except the SPS flag allowing missing frame numbers.
/// Hardware LTR references remain usable when a decoder inserts placeholders
/// for packets deliberately discarded during Moonlight's recovery window.
pub fn h264_reference_recovery(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let prefix = if bytes[at..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[at..].starts_with(&[0, 0, 1]) {
            3
        } else {
            output.push(bytes[at]);
            at += 1;
            continue;
        };
        output.extend_from_slice(&bytes[at..at + prefix]);
        let start = at + prefix;
        let end = (start..bytes.len())
            .find(|&index| {
                bytes[index..].starts_with(&[0, 0, 1]) || bytes[index..].starts_with(&[0, 0, 0, 1])
            })
            .unwrap_or(bytes.len());
        if let Some(sps) = allow_gaps(&bytes[start..end]) {
            output.extend(sps);
        } else {
            output.extend_from_slice(&bytes[start..end]);
        }
        at = end;
    }
    output
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn truncated_and_unrelated_nals_are_unchanged_and_edits_are_idempotent() {
        for length in 0..256 {
            let bytes = vec![0u8; length];
            assert_eq!(h264_reference_recovery(&bytes), bytes);
        }
        let nal = [0, 0, 0, 1, 0x65, 0x88, 0x84, 0, 0, 3, 1];
        assert_eq!(h264_reference_recovery(&nal), nal);
        let sps = [0, 0, 0, 1, 0x67, 0x42, 0, 0x1f, 0xe5, 0x88, 0x68];
        let edited = h264_reference_recovery(&sps);
        assert_ne!(edited, sps);
        assert_eq!(h264_reference_recovery(&edited), edited);
        for end in 0..sps.len() {
            let bytes = &sps[..end];
            let edited = h264_reference_recovery(bytes);
            assert_eq!(h264_reference_recovery(&edited), edited);
        }
    }
}
