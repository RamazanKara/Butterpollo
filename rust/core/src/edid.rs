//! EDID refresh validation with the same range/timing precedence as the host.
use serde::Serialize;
#[derive(Default, Serialize)]
pub struct Refresh {
    pub edid_present: bool,
    pub max_vertical_hz: Option<u32>,
    pub max_timing_hz: Option<f64>,
}
impl Refresh {
    pub fn parse(bytes: &[u8]) -> Self {
        let mut info = Self {
            edid_present: !bytes.is_empty(),
            ..Default::default()
        };
        if bytes.len() < 128 {
            return info;
        }
        info.descriptors(&bytes[54..126]);
        for block in bytes[128..]
            .as_chunks::<128>()
            .0
            .iter()
            .take(usize::from(bytes[126]))
        {
            if block[0] == 2 && (4..127).contains(&block[2]) {
                info.descriptors(&block[usize::from(block[2])..127]);
            }
        }
        info
    }
    fn descriptors(&mut self, bytes: &[u8]) {
        for d in bytes.as_chunks::<18>().0 {
            let clock = u16::from_le_bytes([d[0], d[1]]);
            if clock == 0 {
                if d[3] == 0xfd && d[6] > 0 {
                    self.max_vertical_hz =
                        Some(self.max_vertical_hz.unwrap_or(0).max(u32::from(d[6])));
                }
                continue;
            }
            let width = u32::from(d[2])
                + (u32::from(d[4] & 0xf0) << 4)
                + u32::from(d[3])
                + (u32::from(d[4] & 15) << 8);
            let height = u32::from(d[5])
                + (u32::from(d[7] & 0xf0) << 4)
                + u32::from(d[6])
                + (u32::from(d[7] & 15) << 8);
            if width > 0 && height > 0 {
                let hz = f64::from(clock) * 10000.0 / f64::from(width * height)
                    * if d[17] & 0x80 != 0 { 2.0 } else { 1.0 };
                self.max_timing_hz = Some(self.max_timing_hz.unwrap_or(0.0).max(hz));
            }
        }
    }
    pub fn support(&self, hz: u32) -> (Option<bool>, &'static str) {
        if !self.edid_present {
            return (None, "unknown");
        }
        if self
            .max_vertical_hz
            .is_some_and(|v| f64::from(v) + 0.5 >= f64::from(hz))
        {
            return (Some(true), "range");
        }
        if self.max_timing_hz.is_some_and(|v| v + 0.5 >= f64::from(hz)) {
            return (Some(true), "timing");
        }
        if self.max_vertical_hz.is_some() {
            (Some(false), "range")
        } else if self.max_timing_hz.is_some() {
            (Some(false), "timing")
        } else {
            (None, "unknown")
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detailed_1080p60_and_vertical_range_use_the_retained_tolerance() {
        let mut bytes = vec![0; 128];
        bytes[54..72].copy_from_slice(&[
            2, 58, 128, 24, 113, 56, 45, 64, 88, 44, 69, 0, 0, 0, 0, 0, 0, 0,
        ]);
        bytes[75] = 0xfd;
        bytes[78] = 120;
        let info = Refresh::parse(&bytes);
        assert!((info.max_timing_hz.unwrap() - 60.0).abs() < 0.01);
        assert_eq!(info.support(120), (Some(true), "range"));
        assert_eq!(info.support(180), (Some(false), "range"));
        assert_eq!(Refresh::parse(&[]).support(120), (None, "unknown"));
        for length in 0..256 {
            let _ = Refresh::parse(&vec![0x02; length]);
        }
    }
}
