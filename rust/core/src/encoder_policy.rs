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
/// AVC intra refresh requires two retained references in AMF. A client that
/// negotiated only one must use IDR recovery instead of exceeding its budget.
pub fn amf_intra_refresh(stream: &Negotiated) -> bool {
    stream.intra_refresh && !(stream.codec == 0 && stream.references == 1)
}

/// AMF keeps LTR anchors alongside the rolling short-term reference. The
/// decoder's negotiated budget includes both; zero means it imposed no limit.
/// In particular, Moonlight's one-reference AVC mode cannot retain LTR anchors.
pub fn amf_ltr_frames(config: &Config, stream: &Negotiated) -> usize {
    if stream.intra_refresh {
        return 0;
    }
    let requested = config.integer("amd_ltr_frames", 0).clamp(0, 4) as usize;
    if stream.references == 0 {
        requested
    } else {
        requested.min(stream.references.saturating_sub(1) as usize)
    }
}

/// The capability that counts a codec's encoder engines and the property
/// that lets the driver split one frame across them. H.264 has neither.
pub fn amf_multi_instance(codec: u8) -> Option<(&'static str, &'static str)> {
    match codec {
        1 => Some(("HevcNumOfHwInstances", "HevcMultiHwInstanceEncode")),
        2 => Some(("Av1CapNumOfHwInstances", "Av1MultiHwInstanceEncode")),
        _ => None,
    }
}

/// What to write for split-frame encoding (`amd_split_frame`), given the
/// engines the caps report and the value the driver has before Init. AMD
/// documents the property as a hint: the driver still decides whether a frame
/// is split. `auto` asks for it only where the driver left it off, as the
/// original host did. A single engine gets nothing written in any mode, and a
/// driver that rejects the property keeps the stream.
pub fn amf_split_frame(
    config: &Config,
    codec: u8,
    instances: i64,
    current: Option<bool>,
) -> Option<Property> {
    let (_, name) = amf_multi_instance(codec)?;
    if instances <= 1 {
        return None;
    }
    let on = match tristate(config, "amd_split_frame", None) {
        Some(on) => on,
        None if current == Some(true) => return None,
        None => true,
    };
    Some(Property {
        name: name.into(),
        value: Value::Boolean(on),
        required: false,
    })
}

/// Apply after the frame rate and target bitrate: usage presets derive their
/// buffer sizes from those, unless the user requests an explicit limit.
pub fn amf_rate_control(config: &Config, stream: &Negotiated) -> Result<Vec<Property>> {
    let prefix = match stream.codec {
        0 => "",
        1 => "Hevc",
        2 => "Av1",
        _ => bail!("invalid AMF codec"),
    };
    let bitrate = f64::from(stream.bitrate_kbps) * 1000.;
    let frame_bits = bitrate * 1000. / f64::from(stream.fps_millihz());
    let mut output = Vec::new();
    for (key, suffix, minimum, maximum, scale) in [
        ("amd_peak_bitrate_ratio", "PeakBitrate", 1., 2., bitrate),
        (
            "amd_vbv_buffer_frames",
            "VBVBufferSize",
            0.5,
            2.,
            frame_bits,
        ),
        (
            "amd_max_frame_size",
            if stream.codec == 2 {
                "MaxCompressedFrameSize"
            } else {
                "MaxAUSize"
            },
            1.,
            8.,
            frame_bits,
        ),
    ] {
        let value = config.get(key, "0").trim().trim_matches('"').parse::<f64>();
        let value = match value {
            Ok(0.) => continue,
            Ok(value) if (minimum..=maximum).contains(&value) => value,
            _ => bail!("{key} must be 0 (driver default) or {minimum}–{maximum}"),
        };
        output.push(Property {
            name: format!("{prefix}{suffix}"),
            value: Value::Integer((value * scale).round() as i64),
            required: true,
        });
    }
    Ok(output)
}

pub fn amf(config: &Config, stream: &Negotiated) -> Result<Vec<Property>> {
    let codec = stream.codec;
    let intra_refresh = amf_intra_refresh(stream);
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
        tracing::warn!(
            "AMF quality rate control requires SDR pre-analysis; HDR uses peak VBR instead. Select peak VBR for HDR or disable HDR to use quality rate control"
        );
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
    if stream.hdr && config.boolean("amd_preanalysis", false) {
        tracing::warn!(
            "AMF pre-analysis requires NV12 SDR input; disabled for HDR. Disable HDR to use pre-analysis, or leave pre-analysis disabled for HDR streams"
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
    // Rate control must not drop a frame to stay on budget: a VRR client
    // shows a dropped frame as a held picture. Vibepollo sets it off too.
    // The RX 7900 XT's AV1 encoder has no such property; its settings log
    // line shows the value where a driver has one.
    if codec < 2 {
        add(
            format!("{prefix}RateControlSkipFrameEnable"),
            Value::Boolean(false),
            false,
        );
    }
    // Only on request, as the original backend and FFmpeg do: forcing a queue
    // of one for every VRR client exposed an RDNA4 driver freeze
    // (AlkaidLab/foundation-sunshine#666), video stalling while audio plays.
    let queue = config.integer("amd_input_queue_size", 0).clamp(0, 32);
    if queue > 0 {
        add(
            format!("{prefix}InputQueueSize"),
            Value::Integer(queue),
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
        .max(if intra_refresh && codec == 0 { 2 } else { 0 });
    if references > 0 {
        add(
            format!("{prefix}MaxNumRefFrames"),
            Value::Integer(references as i64),
            true,
        );
    }
    if intra_refresh {
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
        // AVCodecContext.refs limits the active prediction list, while NVENC's
        // retained-picture budget is a separate option. Bound both for clients
        // such as Moonlight's one-reference AVC decoder.
        if stream.references > 0 {
            add("dpb_size", &stream.references.to_string());
        }
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
    fn amf_rate_limits_preserve_driver_defaults_and_use_codec_names_and_bits() {
        for (codec, prefix, cap) in [
            (0, "", "MaxAUSize"),
            (1, "Hevc", "MaxAUSize"),
            (2, "Av1", "MaxCompressedFrameSize"),
        ] {
            let stream = Negotiated {
                codec,
                bitrate_kbps: 40_000,
                fps: 60,
                rate_millihz: 59_940,
                ..Default::default()
            };
            for text in [
                "",
                "amd_peak_bitrate_ratio=0\namd_vbv_buffer_frames=0\namd_max_frame_size=0",
            ] {
                assert!(
                    amf_rate_control(&Config::parse(text).unwrap(), &stream)
                        .unwrap()
                        .is_empty()
                );
            }
            for (peak, vbv, cap_frames, expected) in [
                (1., 0.5, 1., [40_000_000, 333_667, 667_334]),
                (1.5, 1., 2., [60_000_000, 667_334, 1_334_668]),
                (2., 2., 4., [80_000_000, 1_334_668, 2_669_336]),
            ] {
                let config = Config::parse(&format!(
                    "amd_peak_bitrate_ratio={peak}\namd_vbv_buffer_frames={vbv}\namd_max_frame_size={cap_frames}"
                )).unwrap();
                let properties = amf_rate_control(&config, &stream).unwrap();
                for ((property, suffix), expected) in properties
                    .iter()
                    .zip(["PeakBitrate", "VBVBufferSize", cap])
                    .zip(expected)
                {
                    assert_eq!(property.name, format!("{prefix}{suffix}"));
                    assert_eq!(property.value, Value::Integer(expected));
                    assert!(property.required);
                }
            }
        }
    }

    #[test]
    fn amf_rate_limits_reject_invalid_settings() {
        for (key, invalid) in [
            ("amd_peak_bitrate_ratio", "0.9"),
            ("amd_peak_bitrate_ratio", "2.1"),
            ("amd_vbv_buffer_frames", "0.4"),
            ("amd_vbv_buffer_frames", "2.1"),
            ("amd_max_frame_size", "0.5"),
            ("amd_max_frame_size", "8.1"),
        ] {
            for value in [invalid, "-1", "NaN", "inf", "invalid"] {
                let config = Config::parse(&format!("{key}={value}")).unwrap();
                assert!(amf_rate_control(&config, &Negotiated::default()).is_err());
            }
        }
    }

    #[test]
    fn legacy_nvenc_retained_picture_budget_matches_negotiated_references() {
        let config = Config::default();
        for (codec, name) in [(0, "h264_nvenc"), (1, "hevc_nvenc"), (2, "av1_nvenc")] {
            for references in [0, 1, 2, 4, 16] {
                let stream = Negotiated {
                    codec,
                    references,
                    ..Default::default()
                };
                let options = ffmpeg(&config, &stream, name).unwrap();
                let dpb = options.iter().find(|(key, _)| key == "dpb_size");
                assert_eq!(
                    dpb.map(|(_, value)| value.clone()),
                    (references > 0).then(|| references.to_string())
                );
                for other in ["h264_qsv", "libx264", "libx265"] {
                    assert!(
                        !ffmpeg(&config, &stream, other)
                            .unwrap()
                            .iter()
                            .any(|(key, _)| key == "dpb_size")
                    );
                }
            }
        }
    }

    #[test]
    fn amf_ltr_anchors_share_the_negotiated_reference_budget() {
        let config = Config::parse("amd_ltr_frames=4\n").unwrap();
        for codec in 0..=2 {
            for (references, expected_ltr) in [(1, 0), (2, 1), (3, 2), (4, 3), (5, 4), (16, 4)] {
                let stream = Negotiated {
                    codec,
                    references,
                    ..Default::default()
                };
                let count = amf_ltr_frames(&config, &stream);
                assert_eq!(count, expected_ltr);
                assert!(count < references as usize);
                let properties = amf(&config, &stream).unwrap();
                assert!(properties.iter().any(|property| {
                    property.name.ends_with("MaxNumRefFrames")
                        && property.value == Value::Integer(i64::from(references))
                        && property.required
                }));
            }
        }
    }

    #[test]
    fn one_reference_avc_intra_refresh_falls_back_without_expanding_the_budget() {
        let config = Config::parse("amd_ltr_frames=4\n").unwrap();
        for references in [0, 1, 2, 4] {
            let stream = Negotiated {
                references,
                intra_refresh: true,
                ..Default::default()
            };
            let enabled = references != 1;
            assert_eq!(amf_intra_refresh(&stream), enabled);
            let properties = amf(&config, &stream).unwrap();
            assert!(properties.iter().any(|property| {
                property.name == "MaxNumRefFrames"
                    && property.value
                        == Value::Integer(if references == 0 {
                            2
                        } else {
                            i64::from(references)
                        })
            }));
            assert_eq!(
                properties
                    .iter()
                    .any(|property| { property.name == "IntraRefreshMBsNumberPerSlot" }),
                enabled
            );
            assert_eq!(amf_ltr_frames(&config, &stream), 0);
        }
    }

    #[test]
    fn one_reference_avc_uses_idr_recovery_without_ltr_marks() {
        let config = Config::parse("amd_ltr_frames=4\n").unwrap();
        let stream = Negotiated {
            references: 1,
            ..Default::default()
        };
        let mut recovery = crate::ltr::References::new(amf_ltr_frames(&config, &stream));
        for frame in 1..=64 {
            let plan = recovery.plan(frame, frame == 1);
            assert!(plan.mark.is_none());
            assert!(plan.reference.is_none());
            recovery.accepted(frame, &plan);
        }
        assert!(!recovery.enabled());
        assert!(!recovery.invalidate(60, 61));
    }

    #[test]
    fn amf_ltr_preserves_unrestricted_clients_and_explicit_disable() {
        let stream = Negotiated::default();
        for (requested, expected) in [(-1, 0), (0, 0), (1, 1), (4, 4), (99, 4)] {
            let config = Config::parse(&format!("amd_ltr_frames={requested}\n")).unwrap();
            assert_eq!(amf_ltr_frames(&config, &stream), expected);
            assert_eq!(
                amf_ltr_frames(
                    &config,
                    &Negotiated {
                        intra_refresh: true,
                        ..stream.clone()
                    }
                ),
                0
            );
        }
    }

    #[test]
    fn amf_split_frame_hint_follows_the_setting_and_the_engine_count() {
        let request = |text: &str, codec, instances, current| {
            amf_split_frame(&Config::parse(text).unwrap(), codec, instances, current).map(
                |property| {
                    assert!(!property.required);
                    (property.name, property.value)
                },
            )
        };
        let on = |name: &str| Some((name.to_owned(), Value::Boolean(true)));
        let off = |name: &str| Some((name.to_owned(), Value::Boolean(false)));
        for (codec, name) in [
            (1, "HevcMultiHwInstanceEncode"),
            (2, "Av1MultiHwInstanceEncode"),
        ] {
            // Auto asks only where the driver left it off or cannot say.
            for text in ["", "amd_split_frame=auto\n"] {
                assert_eq!(request(text, codec, 2, Some(true)), None);
                assert_eq!(request(text, codec, 2, Some(false)), on(name));
                assert_eq!(request(text, codec, 2, None), on(name));
            }
            for current in [Some(true), Some(false), None] {
                assert_eq!(
                    request("amd_split_frame=enabled\n", codec, 2, current),
                    on(name)
                );
                assert_eq!(
                    request("amd_split_frame=disabled\n", codec, 2, current),
                    off(name)
                );
                // One engine, as the RX 9070 XT's HEVC, or no readable cap.
                for instances in [-1, 0, 1] {
                    for text in [
                        "",
                        "amd_split_frame=enabled\n",
                        "amd_split_frame=disabled\n",
                    ] {
                        assert_eq!(request(text, codec, instances, current), None);
                    }
                }
            }
        }
        // H.264 has no multi-instance property.
        for text in [
            "",
            "amd_split_frame=enabled\n",
            "amd_split_frame=disabled\n",
        ] {
            assert_eq!(request(text, 0, 2, None), None);
        }
        assert_eq!(amf_multi_instance(0), None);
        assert_eq!(
            amf_multi_instance(1),
            Some(("HevcNumOfHwInstances", "HevcMultiHwInstanceEncode"))
        );
        assert_eq!(
            amf_multi_instance(2),
            Some(("Av1CapNumOfHwInstances", "Av1MultiHwInstanceEncode"))
        );
        // The encoder properties written for every stream never include it.
        for codec in 0..=2 {
            let stream = Negotiated {
                codec,
                ..Default::default()
            };
            let config = Config::parse("amd_split_frame=enabled\n").unwrap();
            assert!(
                !amf(&config, &stream)
                    .unwrap()
                    .iter()
                    .any(|property| property.name.contains("MultiHwInstance"))
            );
        }
    }
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
