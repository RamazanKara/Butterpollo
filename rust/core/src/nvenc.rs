//! Reviewed NVENC driver versions and bounded reference recovery.
use crate::{config::Config, rtsp::Negotiated};
use anyhow::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ApiVersion(pub u8, pub u8);
impl ApiVersion {
    pub const COMPILED: Self = Self(13, 0);
    pub const fn packed(self) -> u32 {
        self.0 as u32 | ((self.1 as u32) << 24)
    }
    pub const fn from_driver(value: u32) -> Self {
        Self((value >> 4) as u8, (value & 15) as u8)
    }
    pub const fn structure(self, revision: u32, extended: bool) -> u32 {
        self.packed() | (revision << 16) | (7 << 28) | if extended { 1 << 31 } else { 0 }
    }
    pub fn candidates(codec: u8, maximum: Self) -> impl Iterator<Item = Self> {
        [
            Self(13, 0),
            Self(12, 2),
            Self(12, 1),
            Self(12, 0),
            Self(11, 0),
        ]
        .into_iter()
        .filter(move |v| {
            *v <= maximum && *v <= Self::COMPILED && codec <= 2 && (codec != 2 || *v >= Self(12, 1))
        })
    }
    pub const fn modern_depth(self) -> bool {
        self.0 > 12 || (self.0 == 12 && self.1 >= 2)
    }
    pub const fn initialize(self) -> u32 {
        self.structure(
            if self.modern_depth() {
                7
            } else if self.0 == 12 && self.1 == 1 {
                6
            } else {
                5
            },
            true,
        )
    }
    pub const fn config(self) -> u32 {
        self.structure(
            if self.modern_depth() {
                9
            } else if self.0 == 11 {
                7
            } else {
                8
            },
            true,
        )
    }
    pub const fn preset(self) -> u32 {
        self.structure(if self.modern_depth() { 5 } else { 4 }, true)
    }
    pub const fn reconfigure(self) -> u32 {
        self.structure(if self.modern_depth() { 2 } else { 1 }, true)
    }
    pub const fn register(self) -> u32 {
        self.structure(
            if self.modern_depth() {
                5
            } else if self.0 == 11 {
                3
            } else {
                4
            },
            false,
        )
    }
    pub const fn picture(self) -> u32 {
        self.structure(
            if self.modern_depth() {
                7
            } else if self.0 == 11 {
                4
            } else {
                6
            },
            true,
        )
    }
    pub const fn lock(self) -> u32 {
        self.structure(
            if self.0 == 11 || (self.0 == 12 && self.1 == 1) {
                1
            } else {
                2
            },
            self.0 > 12 || (self.0 == 12 && self.1 >= 1),
        )
    }
    pub const fn event(self) -> u32 {
        self.structure(if self.modern_depth() { 2 } else { 1 }, false)
    }
}

#[derive(Clone, Debug)]
pub struct Tuning {
    pub preset: usize,
    pub multipass: u32,
    pub vbv_increase: u32,
    pub weighted_prediction: bool,
    pub spatial_aq: bool,
    pub temporal_aq: bool,
    pub min_qp: Option<u32>,
    pub cavlc: bool,
    pub filler: bool,
    pub split: u32,
}
impl Tuning {
    pub fn new(config: &Config, stream: &Negotiated) -> Result<Self> {
        let multipass = match config.get("nvenc_twopass", "quarter_res") {
            "disabled" => 0,
            "full_res" => 2,
            other => {
                crate::config::fallback("nvenc_twopass", other, "quarter_res");
                1
            }
        };
        let split = match config.get(
            "nvenc_split_encode",
            config.get("nvenc_force_split_encode", "auto"),
        ) {
            "disabled" | "false" => 15,
            "forced" | "enabled" | "true" => 1,
            // Vibepollo's name for automatic split encoding.
            "driver_decides" => 0,
            other => {
                crate::config::fallback("nvenc_split_encode", other, "auto");
                0
            }
        };
        let min_qp = config.boolean("nvenc_enable_min_qp", false).then(|| {
            let (key, default) = match stream.codec {
                0 => ("nvenc_min_qp_h264", 19),
                1 => ("nvenc_min_qp_hevc", 23),
                _ => ("nvenc_min_qp_av1", 23),
            };
            config
                .integer(key, default)
                .clamp(0, if stream.codec == 2 { 255 } else { 51 }) as u32
        });
        Ok(Self {
            preset: config.integer("nvenc_preset", 1).clamp(1, 7) as usize,
            multipass,
            vbv_increase: config.integer("nvenc_vbv_increase", 0).clamp(0, 400) as u32,
            weighted_prediction: config.boolean("nvenc_weighted_prediction", false),
            spatial_aq: config.boolean("nvenc_spatial_aq", false),
            temporal_aq: config.boolean("nvenc_temporal_aq", false),
            min_qp,
            cavlc: config.boolean("nvenc_h264_cavlc", false),
            filler: config.boolean("nvenc_insert_filler_data", false),
            split,
        })
    }
    pub fn vbv(&self, bitrate_kbps: u32, rate_millihz: u32) -> u32 {
        (u64::from(bitrate_kbps) * 1_000_000 * u64::from(100 + self.vbv_increase)
            / u64::from(rate_millihz.max(1))
            / 100)
            .min(u64::from(u32::MAX)) as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    AlreadyApplied,
    Invalidate { first: u64, last: u64 },
    Idr,
}
#[derive(Default)]
pub struct References {
    last: u64,
    last_idr: u64,
    covered: Option<(u64, u64)>,
    confirmation: bool,
}
impl References {
    pub fn completed(&mut self, frame: u64, idr: bool) {
        self.last = frame;
        if idr {
            self.last_idr = frame;
            self.covered = None;
            self.confirmation = false;
        }
    }
    pub fn plan(&self, first: u64, last: u64, retained: u32, supported: bool) -> Recovery {
        if !supported || first == 0 || last < first || last > self.last {
            return Recovery::Idr;
        }
        // Delayed feedback for an older GOP cannot poison the current one.
        // A lost IDR itself has no retained independent anchor.
        if last < self.last_idr {
            return Recovery::AlreadyApplied;
        }
        if first <= self.last_idr {
            return Recovery::Idr;
        }
        if self
            .covered
            .is_some_and(|(start, end)| first >= start && last <= end)
        {
            return Recovery::AlreadyApplied;
        }
        if self.last.saturating_sub(first).saturating_add(1) >= u64::from(retained) {
            return Recovery::Idr;
        }
        Recovery::Invalidate {
            first,
            last: self.last,
        }
    }
    pub fn applied(&mut self, first: u64, last: u64) {
        self.covered = Some((first, last));
        self.confirmation = true;
    }
    pub fn confirmation(&mut self, idr: bool) -> bool {
        let pending = std::mem::take(&mut self.confirmation);
        pending && !idr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reviewed_driver_negotiation_keeps_older_h264_hevc_and_av1_layouts() {
        assert_eq!(ApiVersion::from_driver(0xc1), ApiVersion(12, 1));
        assert_eq!(
            ApiVersion::candidates(1, ApiVersion(12, 1)).collect::<Vec<_>>(),
            vec![ApiVersion(12, 1), ApiVersion(12, 0), ApiVersion(11, 0)]
        );
        assert_eq!(ApiVersion::candidates(2, ApiVersion(12, 0)).count(), 0);
        assert_eq!(
            ApiVersion::candidates(2, ApiVersion(12, 2)).collect::<Vec<_>>(),
            vec![ApiVersion(12, 2), ApiVersion(12, 1)]
        );
        // SDK 12.2 already changed these revisions; using 12.1 silently breaks 10-bit input.
        assert_eq!(ApiVersion(12, 2).initialize(), 0xf007000c | 2 << 24);
        assert_eq!(ApiVersion(12, 2).config(), 0xf009000c | 2 << 24);
        assert_eq!(ApiVersion(12, 1).lock(), 0xf001000c | 1 << 24);
        assert_eq!(ApiVersion(12, 0).lock(), 0x7002000c);
        assert_eq!(ApiVersion(11, 0).picture(), 0xf004000b);
    }
    #[test]
    fn loss_recovery_includes_dependent_frames_is_bounded_and_confirms_once() {
        let mut references = References::default();
        references.completed(16, false);
        assert_eq!(
            references.plan(14, 14, 5, true),
            Recovery::Invalidate {
                first: 14,
                last: 16
            }
        );
        assert_eq!(references.plan(12, 12, 5, true), Recovery::Idr);
        assert_eq!(references.plan(17, 18, 5, true), Recovery::Idr);
        assert_eq!(references.plan(15, 14, 5, true), Recovery::Idr);
        assert_eq!(references.plan(0, 1, 5, true), Recovery::Idr);
        references.applied(14, 16);
        assert_eq!(references.plan(14, 15, 5, true), Recovery::AlreadyApplied);
        assert!(references.confirmation(false));
        assert!(!references.confirmation(false));
        references.applied(14, 16);
        assert!(!references.confirmation(true));
        references.completed(17, true);
        assert_eq!(references.plan(14, 15, 5, true), Recovery::AlreadyApplied);
        assert_eq!(references.plan(17, 17, 5, true), Recovery::Idr);
        references.completed(19, false);
        assert_eq!(references.plan(16, 18, 5, true), Recovery::Idr);
        assert_eq!(
            references.plan(18, 18, 5, true),
            Recovery::Invalidate {
                first: 18,
                last: 19
            }
        );
        assert_eq!(references.plan(14, 15, 5, false), Recovery::Idr);
    }
}
