//! Settings shared by the native encoders. Property names and enum values follow
//! the retained AMF SDK, rather than assuming all codec names share a prefix.
use crate::{config::Config, rtsp::Negotiated};
use anyhow::{Result, bail};

/// Retired names in existing Apollo/Vibepollo profiles select their current
/// native backend. Keep the saved setting intact while resolving it at use.
/// Names this host cannot use (`mediafoundation`) select automatically.
pub fn canonical_name(name: &str) -> &str {
    match name {
        "amdvce" | "amdvce_experimental" | "amdvce_ffmpeg" | "amdvce_legacy" => "amf",
        "nvenc_experimental" => "nvenc",
        "" | "auto" | "amf" | "nvenc" | "nvenc_legacy" | "quicksync" | "qsv" | "software" => name,
        // Chooses PyroWave's own encoder; other codecs select automatically.
        "pyrowave" => "auto",
        other => {
            crate::config::fallback("encoder", other, "auto");
            "auto"
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Integer(i64),
    Boolean(bool),
}
#[derive(Debug)]
pub struct Property {
    pub name: String,
    pub value: Value,
    pub required: bool,
}
pub fn tristate(config: &Config, key: &str, default: Option<bool>) -> Option<bool> {
    match config
        .values
        .get(key)
        .map(|s| s.trim().to_ascii_lowercase())
    {
        None => default,
        Some(s) => match s.as_str() {
            "1" | "true" | "yes" | "enable" | "enabled" | "on" => Some(true),
            "0" | "false" | "no" | "disable" | "disabled" | "off" => Some(false),
            _ => None,
        },
    }
}
pub fn amf(config: &Config, stream: &Negotiated) -> Result<Vec<Property>> {
    let codec = stream.codec;
    if codec > 2 {
        bail!("invalid AMF codec");
    }
    let prefix = match codec {
        0 => "",
        1 => "Hevc",
        _ => "Av1",
    };
    let mut output = vec![];
    let mut add = |name: String, value: Value, required: bool| {
        output.push(Property {
            name,
            value,
            required,
        })
    };
    let usage = match config.get("amd_usage", "ultralowlatency") {
        "auto" => None,
        "transcoding" => Some(0),
        "lowlatency" => Some(if codec == 2 { 1 } else { 2 }),
        "webcam" => Some(3),
        "high_quality" => Some(4),
        "lowlatency_high_quality" => Some(5),
        other => {
            crate::config::fallback("amd_usage", other, "ultralowlatency");
            Some(if codec == 2 { 2 } else { 1 })
        }
    };
    if let Some(value) = usage {
        add(format!("{prefix}Usage"), Value::Integer(value), true);
    }
    let quality = match config.get("amd_quality", "speed") {
        "auto" => None,
        "balanced" => Some([0, 5, 70][codec as usize]),
        "quality" => Some([2, 0, 30][codec as usize]),
        other => {
            crate::config::fallback("amd_quality", other, "speed");
            Some([1, 10, 100][codec as usize])
        }
    };
    if let Some(value) = quality {
        add(
            format!("{prefix}QualityPreset"),
            Value::Integer(value),
            true,
        );
    }
    let requested_rc = match config.get("amd_rc", "vbr_latency") {
        "auto" => None,
        "cqp" => Some(0),
        "cbr" => Some(if codec == 0 { 1 } else { 3 }),
        "vbr_peak" => Some(2),
        "qvbr" => Some(4),
        "hqvbr" => Some(5),
        "hqcbr" => Some(6),
        other => {
            crate::config::fallback("amd_rc", other, "vbr_latency");
            Some(if codec == 0 { 3 } else { 1 })
        }
    };
    // AMF's PA accepts NV12: retain the previous host's HDR demotion.
    let rc = if stream.hdr && requested_rc.is_some_and(|r| r >= 4) {
        Some(2)
    } else {
        requested_rc
    };
    if let Some(value) = rc {
        add(
            format!("{prefix}RateControlMethod"),
            Value::Integer(value),
            config.values.contains_key("amd_rc") || value >= 4,
        );
    }
    // Quality VBR modes require PA. Keep the original one-frame lookahead.
    let preanalysis =
        !stream.hdr && (config.boolean("amd_preanalysis", false) || rc.is_some_and(|r| r >= 4));
    add(
        format!("{prefix}EnablePreAnalysis"),
        Value::Boolean(preanalysis),
        preanalysis,
    );
    if preanalysis {
        add("PALookAheadBufferDepth".into(), Value::Integer(1), true);
    }
    if rc == Some(4) {
        let q = config.integer("amd_qvbr_quality_level", 0);
        if (1..=51).contains(&q) {
            add(format!("{prefix}QvbrQualityLevel"), Value::Integer(q), true);
        } else if q != 0 {
            crate::config::invalid("amd_qvbr_quality_level", &q.to_string());
        }
    }
    if let Some(on) = tristate(config, "amd_vbaq", Some(true)) {
        let on = on && rc != Some(0);
        if codec == 2 {
            add("Av1AQMode".into(), Value::Integer(i64::from(on)), true);
        } else {
            add(format!("{prefix}EnableVBAQ"), Value::Boolean(on), true);
        }
    }
    add(
        format!("{prefix}EnforceHRD"),
        Value::Boolean(config.boolean("amd_enforce_hrd", false)),
        config.values.contains_key("amd_enforce_hrd"),
    );
    let queue = config.integer("amd_input_queue_size", 0).clamp(0, 32);
    if queue > 0 || stream.vrr_low_latency {
        add(
            format!("{prefix}InputQueueSize"),
            Value::Integer(if queue == 0 { 1 } else { queue }),
            true,
        );
    }
    if let Some(on) = tristate(config, "amd_smart_access_video", None) {
        add(
            format!("{prefix}EnableEncoderSmartAccessVideo"),
            Value::Boolean(on),
            on,
        );
    }
    if codec < 2
        && let Some(on) = tristate(config, "amd_lowlatency_mode", None)
    {
        add("LowLatencyInternal".into(), Value::Boolean(on), true);
    }
    if let Some(on) = tristate(config, "amd_high_motion_quality_boost", None) {
        add(
            match codec {
                0 => "HighMotionQualityBoostEnable",
                1 => "HevcHighMotionQualityBoostEnable",
                _ => "Av1HighMotionQualityBoost",
            }
            .into(),
            Value::Boolean(on),
            false,
        );
    }
    if codec == 0 {
        add("Profile".into(), Value::Integer(100), true);
        match config.get("amd_coder", "auto") {
            "cabac" | "ac" => add("CABACEnable".into(), Value::Integer(1), true),
            "cavlc" | "vlc" => add("CABACEnable".into(), Value::Integer(2), true),
            other => crate::config::fallback("amd_coder", other, "auto"),
        }
    }
    if codec == 2 {
        if let Some(on) = tristate(config, "amd_av1_screen_content", None) {
            add("Av1ScreenContentTools".into(), Value::Boolean(on), true);
        }
        let mode = match config.get("amd_av1_latency_mode", "auto") {
            "none" => Some(0),
            "power_saving" => Some(1),
            "realtime" => Some(2),
            "lowest" => Some(3),
            other => {
                crate::config::fallback("amd_av1_latency_mode", other, "auto");
                None
            }
        };
        if let Some(mode) = mode {
            add("Av1EncodingLatencyMode".into(), Value::Integer(mode), true);
        }
        let tiles = config.integer("amd_av1_tiles", 0);
        if !matches!(tiles, 0 | 1 | 2 | 4) {
            bail!("amd_av1_tiles must be 0, 1, 2 or 4");
        }
        if tiles > 0 || stream.slices > 1 {
            add(
                "Av1NumTilesPerFrame".into(),
                Value::Integer(if tiles == 0 {
                    stream.slices.min(4) as i64
                } else {
                    tiles
                }),
                true,
            );
        }
    } else if stream.slices > 1 {
        add(
            format!("{prefix}SlicesPerFrame"),
            Value::Integer(stream.slices as i64),
            true,
        );
    }
    let references = stream
        .references
        .max(if stream.intra_refresh && codec == 0 {
            2
        } else {
            0
        });
    if references > 0 {
        add(
            format!("{prefix}MaxNumRefFrames"),
            Value::Integer(references as i64),
            true,
        );
    }
    if stream.intra_refresh {
        if codec == 2 {
            add("Av1IntraRefreshMode".into(), Value::Integer(2), true);
            add(
                "Av1IntraRefreshNumOfStripes".into(),
                Value::Integer(300),
                true,
            );
        } else {
            let block = if codec == 0 { 16 } else { 64 };
            let blocks = stream.width.div_ceil(block) * stream.height.div_ceil(block);
            add(
                if codec == 0 {
                    "IntraRefreshMBsNumberPerSlot"
                } else {
                    "HevcIntraRefreshCTBsNumberPerSlot"
                }
                .into(),
                Value::Integer(blocks.div_ceil(blocks.clamp(1, 299)) as i64),
                true,
            );
        }
    }
    Ok(output)
}

/// FFmpeg SDK options, including the old native NVENC and QSV controls.
pub fn ffmpeg(config: &Config, stream: &Negotiated, name: &str) -> Result<Vec<(String, String)>> {
    let mut output = vec![];
    let mut add = |key: &str, value: &str| output.push((key.into(), value.into()));
    if name.ends_with("_nvenc") {
        add(
            "preset",
            &format!("p{}", config.integer("nvenc_preset", 1).clamp(1, 7)),
        );
        add("tune", "ull");
        add("rc", "cbr");
        add("zerolatency", "1");
        add("forced-idr", "1");
        add("delay", "0");
        add(
            "spatial-aq",
            if config.boolean("nvenc_spatial_aq", false) {
                "1"
            } else {
                "0"
            },
        );
        add(
            "temporal-aq",
            if config.boolean("nvenc_temporal_aq", false) {
                "1"
            } else {
                "0"
            },
        );
        add(
            "multipass",
            match config.get("nvenc_twopass", "quarter_res") {
                "disabled" => "disabled",
                "full_res" => "fullres",
                other => {
                    crate::config::fallback("nvenc_twopass", other, "quarter_res");
                    "qres"
                }
            },
        );
        if stream.codec == 0 {
            add(
                "coder",
                if config.boolean("nvenc_h264_cavlc", false) {
                    "cavlc"
                } else {
                    "cabac"
                },
            );
        }
        if config.values.contains_key("nvenc_split_encode")
            || config.values.contains_key("nvenc_force_split_encode")
        {
            let mode = config.get(
                "nvenc_split_encode",
                config.get("nvenc_force_split_encode", "auto"),
            );
            add(
                "split_encode_mode",
                match mode {
                    "disabled" | "false" => "15",
                    "forced" | "enabled" | "true" => "2",
                    "driver_decides" => "0",
                    other => {
                        crate::config::fallback("nvenc_split_encode", other, "auto");
                        "0"
                    }
                },
            );
        }
        if stream.intra_refresh {
            add("intra-refresh", "1");
        }
    } else if name.ends_with("_qsv") {
        add("preset", config.get("qsv_preset", "medium"));
        add("async_depth", "1");
        add("low_delay_brc", "1");
        add("forced_idr", "1");
        if stream.codec == 1 {
            add(
                "low_power",
                if config.boolean("qsv_slow_hevc", false) {
                    "0"
                } else {
                    "1"
                },
            );
        }
        if stream.codec == 0 {
            match config.get("qsv_coder", "auto") {
                "cabac" | "ac" => add("cavlc", "0"),
                "cavlc" | "vlc" => add("cavlc", "1"),
                other => crate::config::fallback("qsv_coder", other, "auto"),
            }
        }
        if stream.intra_refresh {
            add("int_ref_type", "vertical");
            add("int_ref_cycle_size", "300");
        }
    } else if matches!(name, "libx264" | "libx265") {
        add("preset", config.get("sw_preset", "superfast"));
        add("tune", config.get("sw_tune", "zerolatency"));
        if name == "libx264" {
            add(
                "x264-params",
                if stream.intra_refresh {
                    "repeat-headers=1:annexb=1:scenecut=0:intra-refresh=1"
                } else {
                    "repeat-headers=1:annexb=1:scenecut=0"
                },
            );
        } else {
            add("x265-params", "repeat-headers=1:annexb=1:log-level=error");
        }
    } else if name == "libsvtav1" {
        add(
            "preset",
            match config.get("sw_preset", "superfast") {
                "veryslow" => "1",
                "slower" => "2",
                "slow" => "4",
                "medium" => "5",
                "fast" => "7",
                "faster" => "9",
                "veryfast" => "10",
                "superfast" => "11",
                "ultrafast" => "12",
                _ => "11",
            },
        );
        add("svtav1-params", "pred-struct=1:lookahead=0");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imported_encoder_names_keep_the_selected_vendor_and_legacy_backend() {
        for name in [
            "amdvce",
            "amdvce_experimental",
            "amdvce_ffmpeg",
            "amdvce_legacy",
            "amf",
        ] {
            let config = Config::parse(&format!("encoder={name}\n")).unwrap();
            assert_eq!(canonical_name(config.get("encoder", "auto")), "amf");
            assert_eq!(config.get("encoder", "auto"), name);
        }
        assert_eq!(canonical_name("nvenc_experimental"), "nvenc");
        for name in [
            "",
            "auto",
            "nvenc",
            "nvenc_legacy",
            "quicksync",
            "qsv",
            "software",
        ] {
            assert_eq!(canonical_name(name), name);
        }
        for unusable in ["mediafoundation", "unknown", "pyrowave"] {
            assert_eq!(canonical_name(unusable), "auto");
        }
    }
    #[test]
    fn legacy_codec_specific_settings_and_auto_are_preserved() {
        let config = Config::parse("amd_usage=lowlatency\namd_quality=balanced\namd_rc=qvbr\namd_qvbr_quality_level=20\namd_vbaq=disabled\namd_av1_latency_mode=lowest\n").unwrap();
        let mut stream = Negotiated::default();
        let h264 = amf(&config, &stream).unwrap();
        assert!(
            h264.iter()
                .any(|p| p.name == "Usage" && p.value == Value::Integer(2))
        );
        assert!(
            h264.iter()
                .any(|p| p.name == "EnablePreAnalysis" && p.value == Value::Boolean(true))
        );
        stream.codec = 2;
        let av1 = amf(&config, &stream).unwrap();
        assert!(
            av1.iter()
                .any(|p| p.name == "Av1Usage" && p.value == Value::Integer(1))
        );
        assert!(
            av1.iter()
                .any(|p| p.name == "Av1AQMode" && p.value == Value::Integer(0))
        );
        assert!(
            av1.iter()
                .any(|p| p.name == "Av1EncodingLatencyMode" && p.value == Value::Integer(3))
        );
        let auto = amf(
            &Config::parse("amd_quality=auto\namd_vbaq=auto\n").unwrap(),
            &stream,
        )
        .unwrap();
        assert!(
            !auto
                .iter()
                .any(|p| p.name.ends_with("QualityPreset") || p.name == "Av1AQMode")
        );
        for codec in 0..=2 {
            stream.codec = codec;
            for (mode, expected) in [("high_quality", 4), ("lowlatency_high_quality", 5)] {
                let properties = amf(
                    &Config::parse(&format!("amd_usage={mode}\n")).unwrap(),
                    &stream,
                )
                .unwrap();
                assert!(
                    properties
                        .iter()
                        .any(|p| p.name.ends_with("Usage") && p.value == Value::Integer(expected))
                );
            }
        }
    }
}
