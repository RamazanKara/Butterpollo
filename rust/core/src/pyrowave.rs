//! PyroWave's client container and its transport-aware intra-frame budget.
use anyhow::{Result, bail};
pub const PACKET_BOUNDARY: usize = 0x8000;
pub const MAX_BLOCK_BYTES: usize = 16380;
pub fn budget(kbps: u32, fps: u32, payload: usize, fec: usize, minimum: usize) -> Result<usize> {
    if fps == 0 || payload == 0 || fec > 100 || minimum >= 255 {
        bail!("invalid PyroWave transport budget");
    }
    let data = (255 * 100 / (100 + fec)).min(255 - minimum);
    let limit = (4 * data * payload).saturating_sub(8);
    let overhead = 16 + 4 * (limit / (PACKET_BOUNDARY - MAX_BLOCK_BYTES) + 2);
    let allowed = limit.saturating_sub(overhead) & !3;
    if allowed < 4096 {
        bail!("video packet size leaves no usable PyroWave budget");
    }
    Ok(((u64::from(kbps) * 1000 / 8 / u64::from(fps)).max(4096) as usize & !3).min(allowed))
}
pub fn container(bitstream: &[u8], packets: &[(usize, usize)], hdr: bool) -> Result<Vec<u8>> {
    if packets.is_empty() || packets.len() > 65535 {
        bail!("invalid PyroWave packet count");
    }
    let mut size = 8usize;
    for &(offset, length) in packets {
        if length == 0
            || length > u32::MAX as usize
            || offset > bitstream.len()
            || length > bitstream.len() - offset
        {
            bail!("invalid PyroWave packet slice");
        }
        size = size
            .checked_add(4 + length)
            .ok_or_else(|| anyhow::anyhow!("PyroWave container overflow"))?;
    }
    if size > 64 * 1024 * 1024 {
        bail!("PyroWave frame exceeds the limit");
    }
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"PYRW\x01");
    out.extend_from_slice(&(packets.len() as u16).to_be_bytes());
    out.push(u8::from(hdr));
    for &(offset, length) in packets {
        out.extend_from_slice(&(length as u32).to_be_bytes());
        out.extend_from_slice(&bitstream[offset..offset + length]);
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_container_golden_and_checked_slices() {
        assert_eq!(
            container(&[10, 11, 12, 13], &[(1, 2), (0, 1)], true).unwrap(),
            b"PYRW\x01\x00\x02\x01\x00\x00\x00\x02\x0b\x0c\x00\x00\x00\x01\x0a"
        );
        assert!(container(&[1], &[(usize::MAX, 1)], false).is_err());
        assert!(container(&[1], &[(0, usize::MAX)], false).is_err());
        assert!(budget(500000, 30, 240, 100, 255).is_err());
        assert_eq!(budget(2000, 30, 1008, 20, 0).unwrap(), 8332);
        assert!(budget(500000, 30, 1008, 20, 0).unwrap() < 4 * 212 * 1008);
    }
}
