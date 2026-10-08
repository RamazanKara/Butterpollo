use super::*;

#[test]
fn smart_access_video_excludes_only_forced_low_latency() -> Result<()> {
    use butterpollo_core::{config::Config, encoder_policy, rtsp::Negotiated};
    for codec in 0..=2 {
        for low_latency in ["auto", "enabled", "disabled"] {
            for sav in ["auto", "enabled", "disabled"] {
                let config = Config::parse(&format!(
                    "amd_lowlatency_mode={low_latency}\namd_smart_access_video={sav}\n"
                ))?;
                let stream = Negotiated {
                    codec,
                    ..Default::default()
                };
                let original = encoder_policy::amf(&config, &stream)?;
                let mut guarded = encoder_policy::amf(&config, &stream)?;
                guard_smart_access_video(&mut guarded);
                assert_eq!(original.len(), guarded.len());
                for (before, after) in original.iter().zip(&guarded) {
                    assert_eq!(before.name, after.name);
                    if codec < 2
                        && low_latency == "enabled"
                        && sav == "enabled"
                        && before.name.ends_with("EnableEncoderSmartAccessVideo")
                    {
                        assert_eq!(after.value, encoder_policy::Value::Boolean(false));
                        assert!(after.required);
                    } else {
                        assert_eq!(before.value, after.value, "{}", before.name);
                        assert_eq!(before.required, after.required, "{}", before.name);
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires AMD AMF and an independent FFprobe"]
fn hdr10_metadata_and_range_reach_the_bitstream() -> Result<()> {
    use crate::capture::{ComGuard, Pixel};
    use std::{os::windows::process::CommandExt, process::Command};
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let ffprobe =
        std::env::var_os("BUTTERPOLLO_TEST_FFPROBE").context("set BUTTERPOLLO_TEST_FFPROBE")?;
    let directory = tempfile::tempdir()?;
    let (width, height) = (1920u32, 1080u32);
    let pixels = (0..width * height)
        .flat_map(|index| {
            let value = half::f16::from_f32([0.05, 1., 6.][(index % width / 640) as usize]);
            [value, value, value, half::f16::ONE]
        })
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let source = GpuImage::upload(
        &gpu,
        &Image {
            width,
            height,
            stride: width as usize * 8,
            bytes: pixels,
            captured: Instant::now(),
            pixel: Pixel::RgbaF16,
        },
    )?;
    let mut metadata = butterpollo_core::hdr::Metadata::display(1000., 0.005, 400.);
    metadata.max_cll = 1000;
    metadata.max_fall = 400;
    let options = butterpollo_core::config::Config::parse(
        "amd_usage=ultralowlatency\namd_quality=speed\namd_smart_access_video=disabled\n",
    )?;
    for codec in [1u8, 2] {
        for full_range in [false, true] {
            for compute in [false, true] {
                let config = butterpollo_core::rtsp::Negotiated {
                    width,
                    height,
                    codec,
                    hdr: true,
                    csc_mode: 4 | u8::from(full_range),
                    fps: 120,
                    bitrate_kbps: 40000,
                    ..Default::default()
                };
                let queue = compute
                    .then(|| crate::compute::Compute::for_device(&gpu.device))
                    .transpose()?;
                let mut encoder = Encoder::new_gpu(&config, gpu.clone(), &options, queue)?;
                // As the stream does before its first frame.
                encoder.set_hdr_metadata(metadata);
                let mut output = vec![];
                for frame in 0..4 {
                    output.extend(encoder.encode_gpu(&source, frame == 0, config.bitrate_kbps)?);
                }
                let deadline = Instant::now() + Duration::from_secs(3);
                while encoder.pending() {
                    output.extend(encoder.poll()?);
                    if Instant::now() >= deadline {
                        bail!("HDR metadata probe output timed out");
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                let name = format!("codec{codec}-full{full_range}-compute{compute}");
                let bitstream = directory.path().join(format!("{name}.bin"));
                std::fs::write(
                    &bitstream,
                    output
                        .iter()
                        .flat_map(|packet| packet.bytes.iter().copied())
                        .collect::<Vec<_>>(),
                )?;
                let probed = Command::new(&ffprobe)
                    .args([
                        "-v",
                        "error",
                        "-f",
                        if codec == 1 { "hevc" } else { "obu" },
                        "-i",
                    ])
                    .arg(&bitstream)
                    .args(["-show_frames", "-of", "json"])
                    .creation_flags(0x08000000)
                    .output()?;
                assert!(
                    probed.status.success(),
                    "{}",
                    String::from_utf8_lossy(&probed.stderr)
                );
                let probed: serde_json::Value = serde_json::from_slice(&probed.stdout)?;
                let first = &probed["frames"][0];
                assert_eq!(
                    first["color_range"],
                    if full_range { "pc" } else { "tv" },
                    "{name}: {first}"
                );
                assert_eq!(first["color_transfer"], "smpte2084", "{name}: {first}");
                // The matrix is left to the driver to derive from the colour
                // profile; a client converting with BT.709 would shift colours.
                assert_eq!(first["color_primaries"], "bt2020", "{name}: {first}");
                assert_eq!(first["color_space"], "bt2020nc", "{name}: {first}");
                let side = first["side_data_list"]
                    .as_array()
                    .with_context(|| format!("{name}: no side data in {first}"))?;
                let mastering = side
                    .iter()
                    .find(|s| s["side_data_type"] == "Mastering display metadata")
                    .with_context(|| format!("{name}: no mastering display metadata"))?;
                // 1000 nits and BT.2020 red (0.708) in each codec's units.
                let (luminance, red) = if codec == 1 {
                    ("10000000/10000", "35400/50000")
                } else {
                    ("256000/256", "46399/65536")
                };
                assert_eq!(mastering["max_luminance"], luminance, "{name}: {mastering}");
                assert_eq!(mastering["red_x"], red, "{name}: {mastering}");
                let light = side
                    .iter()
                    .find(|s| s["side_data_type"] == "Content light level metadata")
                    .with_context(|| format!("{name}: no content light level"))?;
                assert_eq!(light["max_content"], 1000, "{name}");
                assert_eq!(light["max_average"], 400, "{name}");
            }
        }
    }
    Ok(())
}
#[test]
#[ignore = "requires AMD AMF, independent FFprobe and an artifact directory"]
fn native_av1_geometry_and_hdr_are_preserved() -> Result<()> {
    use crate::capture::{ComGuard, Pixel};
    use std::{os::windows::process::CommandExt, path::PathBuf, process::Command};
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let directory = PathBuf::from(
        std::env::var_os("BUTTERPOLLO_TEST_GEOMETRY_DIRECTORY")
            .context("set BUTTERPOLLO_TEST_GEOMETRY_DIRECTORY")?,
    );
    std::fs::create_dir_all(&directory)?;
    let ffprobe =
        std::env::var_os("BUTTERPOLLO_TEST_FFPROBE").context("set BUTTERPOLLO_TEST_FFPROBE")?;
    let ffmpeg =
        std::env::var_os("BUTTERPOLLO_TEST_FFMPEG").context("set BUTTERPOLLO_TEST_FFMPEG")?;
    let options = butterpollo_core::config::Config::parse(
        "amd_usage=ultralowlatency\namd_quality=speed\namd_smart_access_video=disabled\n",
    )?;
    let mut reports = vec![];
    let mut exact = true;
    for (width, height) in [(1920, 1080), (1968, 2184), (2184, 1968)] {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 8);
        for y in 0..height {
            for x in 0..width {
                let value = [0.1, 0.5, 1., 4.]
                    [usize::from(x >= width / 2) + 2 * usize::from(y >= height / 2)];
                for value in [value, value, value, 1.] {
                    pixels.extend_from_slice(&half::f16::from_f32(value).to_le_bytes());
                }
            }
        }
        let source = GpuImage::upload(
            &gpu,
            &Image {
                width,
                height,
                stride: width as usize * 8,
                bytes: pixels,
                captured: Instant::now(),
                pixel: Pixel::RgbaF16,
            },
        )?;
        for hdr in [false, true] {
            let source = if hdr {
                source.clone()
            } else {
                let bytes = (0..width * height)
                    .flat_map(|index| {
                        let x = index % width;
                        let y = index / width;
                        let value = [25, 128, 255, 255]
                            [usize::from(x >= width / 2) + 2 * usize::from(y >= height / 2)];
                        [value, value, value, 255]
                    })
                    .collect();
                GpuImage::upload(
                    &gpu,
                    &Image {
                        width,
                        height,
                        stride: width as usize * 4,
                        bytes,
                        captured: Instant::now(),
                        pixel: Pixel::Bgra8,
                    },
                )?
            };
            for alignment in [3, 4] {
                let config = butterpollo_core::rtsp::Negotiated {
                    width,
                    height,
                    codec: 2,
                    hdr,
                    fps: 120,
                    bitrate_kbps: 80000,
                    ..Default::default()
                };
                let result = Encoder::new_device_alignment_options(
                    &config,
                    gpu.clone(),
                    &options,
                    alignment,
                );
                let mut encoder = match result {
                    Ok(encoder) => encoder,
                    Err(error) => {
                        reports.push(serde_json::json!({"width":width,"height":height,"hdr":hdr,"alignment":alignment,"init_error":format!("{error:#}")}));
                        exact &= alignment != 3;
                        continue;
                    }
                };
                let read = |name: &str| -> Result<AMFVariantStruct> {
                    let mut value = int(0);
                    unsafe {
                        check(((*(*encoder.component).pVtbl).GetProperty.unwrap())(
                            encoder.component,
                            wide(name).as_ptr(),
                            &mut value,
                        ))?;
                    }
                    Ok(value)
                };
                let applied_alignment = read("Av1AlignmentMode")?;
                let applied_size = read("Av1FrameSize")?;
                let properties = unsafe {
                    serde_json::json!({
                        "alignment_variant":applied_alignment.type_,
                        "alignment":applied_alignment.__bindgen_anon_1.int64Value,
                        "frame_size_variant":applied_size.type_,
                        "width":applied_size.__bindgen_anon_1.sizeValue.width,
                        "height":applied_size.__bindgen_anon_1.sizeValue.height,
                    })
                };
                let mut output = vec![];
                for frame in 0..8 {
                    output.extend(encoder.encode_gpu(&source, frame == 0, config.bitrate_kbps)?);
                }
                let deadline = Instant::now() + Duration::from_secs(3);
                while encoder.pending() {
                    output.extend(encoder.poll()?);
                    if Instant::now() >= deadline {
                        bail!("AV1 geometry probe output timed out");
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(output.len(), 8);
                let name = format!("av1-{width}x{height}-hdr{hdr}-align{alignment}");
                let bitstream = directory.join(format!("{name}.obu"));
                std::fs::write(
                    &bitstream,
                    output
                        .iter()
                        .flat_map(|packet| packet.bytes.iter().copied())
                        .collect::<Vec<_>>(),
                )?;
                let decoded = Command::new(&ffprobe)
                    .args(["-v", "error", "-f", "obu", "-i"])
                    .arg(&bitstream)
                    .args(["-show_frames", "-show_streams", "-of", "json"])
                    .creation_flags(0x08000000)
                    .output()?;
                assert!(
                    decoded.status.success(),
                    "{}",
                    String::from_utf8_lossy(&decoded.stderr)
                );
                let decoded: serde_json::Value = serde_json::from_slice(&decoded.stdout)?;
                std::fs::write(
                    directory.join(format!("{name}.decoded.json")),
                    serde_json::to_vec_pretty(&decoded)?,
                )?;
                let frames = decoded["frames"]
                    .as_array()
                    .context("no independently decoded AV1 frames")?;
                let matches = frames.len() == 8
                    && frames.iter().all(|frame| {
                        frame["width"] == width
                            && frame["height"] == height
                            && (!hdr
                                || (frame["color_primaries"] == "bt2020"
                                    && frame["color_transfer"] == "smpte2084"))
                    });
                exact &= alignment != 3 || matches;
                let trace = Command::new(&ffmpeg)
                    .args(["-hide_banner", "-nostdin", "-f", "obu", "-i"])
                    .arg(&bitstream)
                    .args([
                        "-map",
                        "0:v",
                        "-frames:v",
                        "1",
                        "-c:v",
                        "copy",
                        "-bsf:v",
                        "trace_headers",
                        "-f",
                        "null",
                        "-",
                    ])
                    .creation_flags(0x08000000)
                    .output()?;
                assert!(trace.status.success(), "bitstream trace failed");
                std::fs::write(directory.join(format!("{name}.headers.log")), trace.stderr)?;
                reports.push(serde_json::json!({"width":width,"height":height,"hdr":hdr,"alignment":alignment,
                    "properties":properties,"exact_decoded_geometry_and_hdr":matches,"first_frame":frames.first()}));
            }
        }
    }
    std::fs::write(
        directory.join("geometry.json"),
        serde_json::to_vec_pretty(&reports)?,
    )?;
    assert!(
        exact,
        "AV1 exact decoded geometry failed; inspect geometry.json and encoded headers"
    );
    Ok(())
}

#[test]
#[ignore = "requires AMD AMF, independent FFmpeg and a reference-budget report path"]
fn native_h264_one_reference_budget_decodes_without_ltr_corruption() -> Result<()> {
    use crate::capture::{ComGuard, Pixel};
    use std::{os::windows::process::CommandExt, path::PathBuf, process::Command};
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let decoder = std::env::var_os("BUTTERPOLLO_TEST_FFMPEG")
        .context("set BUTTERPOLLO_TEST_FFMPEG to an independent decoder")?;
    let report = PathBuf::from(
        std::env::var_os("BUTTERPOLLO_TEST_REFERENCE_BUDGET_REPORT")
            .context("set BUTTERPOLLO_TEST_REFERENCE_BUDGET_REPORT")?,
    );
    let directory = report
        .parent()
        .context("report requires a parent directory")?;
    std::fs::create_dir_all(directory)?;
    let config = butterpollo_core::rtsp::Negotiated {
        width: 640,
        height: 480,
        references: 1,
        ..Default::default()
    };
    let options = butterpollo_core::config::Config::parse(
        "amd_ltr_frames=4\namd_usage=ultralowlatency\namd_quality=speed\namd_lowlatency_mode=enabled\n",
    )?;
    // Synthetic input avoids capturing or changing any user's desktop/window.
    let source = GpuImage::upload(
        &gpu,
        &Image {
            width: config.width,
            height: config.height,
            stride: config.width as usize * 4,
            bytes: (0..config.width * config.height)
                .flat_map(|index| {
                    let x = index % config.width;
                    let y = index / config.width;
                    [x as u8, y as u8, (x ^ y) as u8, 255]
                })
                .collect(),
            captured: Instant::now(),
            pixel: Pixel::Bgra8,
        },
    )?;
    let mut encoder = Encoder::new_device_options(&config, gpu, &options)?;
    let supports_invalidation = encoder.supports_invalidation();
    let mut output = vec![];
    for frame in 0..64 {
        output.extend(encoder.encode_gpu(&source, frame == 0, config.bitrate_kbps)?);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while encoder.pending() {
        output.extend(encoder.poll()?);
        if Instant::now() > deadline {
            bail!("AMF reference-budget fixture output timed out");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let bitstream = directory.join(format!("amf-one-reference-{}.h264", std::process::id()));
    std::fs::write(
        &bitstream,
        output
            .iter()
            .flat_map(|packet| packet.bytes.iter().copied())
            .collect::<Vec<_>>(),
    )?;
    // Inspect the encoder's actual SPS independently. A software-only decode
    // can pass an oversized SPS that Moonlight's one-reference fixup would reject.
    let trace = Command::new(&decoder)
        .args(["-hide_banner", "-nostdin", "-f", "h264", "-i"])
        .arg(&bitstream)
        .args([
            "-map",
            "0:v",
            "-c:v",
            "copy",
            "-bsf:v",
            "trace_headers",
            "-f",
            "null",
            "-",
        ])
        .creation_flags(0x08000000)
        .output()?;
    let headers = String::from_utf8_lossy(&trace.stderr);
    let values = |field: &str| -> Vec<u32> {
        headers
            .lines()
            .filter(|line| line.contains(field))
            .filter_map(|line| line.rsplit_once('=')?.1.trim().parse().ok())
            .collect()
    };
    let max_num_ref_frames = values("max_num_ref_frames");
    let max_dec_frame_buffering = values("max_dec_frame_buffering");
    std::fs::write(bitstream.with_extension("headers.log"), &trace.stderr)?;
    let decoded = Command::new(&decoder)
        .args([
            "-hide_banner",
            "-nostdin",
            "-v",
            "error",
            "-xerror",
            "-err_detect",
            "explode",
            "-f",
            "h264",
            "-i",
        ])
        .arg(&bitstream)
        .args([
            "-an",
            "-progress",
            "pipe:1",
            "-nostats",
            "-fps_mode",
            "passthrough",
            "-f",
            "null",
            "-",
        ])
        .creation_flags(0x08000000)
        .output()?;
    std::fs::write(bitstream.with_extension("decode.log"), &decoded.stderr)?;
    let decoded_frames = String::from_utf8_lossy(&decoded.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("frame=")?.trim().parse::<u32>().ok())
        .next_back()
        .unwrap_or(0);
    let observation = serde_json::json!({
        "negotiated_references": config.references,
        "requested_ltr_frames": 4,
        "supports_invalidation": supports_invalidation,
        "applied_max_num_ref_frames": encoder.read("MaxNumRefFrames"),
        "applied_max_ltr_frames": encoder.read("MaxOfLTRFrames"),
        "encoded_frames": output.len(),
        "decoded_frames": decoded_frames,
        "sps_max_num_ref_frames": max_num_ref_frames,
        "vui_max_dec_frame_buffering": max_dec_frame_buffering,
        "header_trace_success": trace.status.success(),
        "decode_success": decoded.status.success(),
        "decode_errors": String::from_utf8_lossy(&decoded.stderr),
        "bitstream": bitstream,
    });
    std::fs::write(&report, serde_json::to_vec_pretty(&observation)?)?;
    eprintln!("{observation}");
    assert!(
        !supports_invalidation,
        "one-reference clients must use IDR recovery"
    );
    assert_eq!(output.len(), 64);
    assert!(trace.status.success(), "independent SPS trace failed");
    assert!(
        !max_num_ref_frames.is_empty(),
        "trace contained no SPS reference count"
    );
    assert!(
        max_num_ref_frames.iter().all(|&count| count <= 1),
        "oversized SPS reference budget"
    );
    assert!(
        max_dec_frame_buffering.iter().all(|&count| count <= 1),
        "oversized VUI decoder buffer budget"
    );
    assert!(
        decoded.status.success() && decoded.stderr.is_empty(),
        "independent decoder rejected stream"
    );
    assert_eq!(decoded_frames, 64);
    Ok(())
}

#[test]
#[ignore = "requires a live AMD D3D11 adapter and AMF runtime"]
fn native_reference_recovery_decodes_after_dropping_the_invalidated_packets() -> Result<()> {
    let _com = crate::capture::ComGuard::new()?;
    let mut capture = crate::capture::Capture::new_format("", "wgc", false)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let image = loop {
        if let Some(image) = capture.next_gpu()? {
            break image;
        }
        if Instant::now() > deadline {
            bail!("capture fixture timed out");
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let options = butterpollo_core::config::Config::parse("amd_ltr_frames=4\n")?;
    let mut reports = vec![];
    for codec in 0..3 {
        let config = butterpollo_core::rtsp::Negotiated {
            width: 640,
            height: 480,
            codec,
            ..Default::default()
        };
        let mut encoder = Encoder::new_device_options(&config, image.gpu.clone(), &options)?;
        let supported = encoder.supports_invalidation();
        let mut output = vec![];
        for frame in 1..=64 {
            let mut idr = frame == 1;
            if frame == 9 && supported {
                assert!(encoder.invalidate_ref_frames(5, 8));
            }
            if frame == 21 && supported {
                idr = !encoder.invalidate_ref_frames(17, 20);
                assert_eq!(idr, codec == 0);
            }
            output.extend(encoder.encode_gpu(&image, idr, config.bitrate_kbps)?);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while encoder.pending() {
            output.extend(encoder.poll()?);
            if Instant::now() > deadline {
                bail!("AMF output fixture timed out");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(output.len(), 64);
        if supported {
            assert!(output[8].after_invalidation);
            assert!(!output[8].idr);
            assert_eq!(output[20].after_invalidation, codec != 0);
            assert_eq!(output[20].idr, codec == 0);
        }
        use std::os::windows::process::CommandExt;
        let decoder = std::env::var_os("BUTTERPOLLO_TEST_FFMPEG")
            .context("set BUTTERPOLLO_TEST_FFMPEG to an independent decoder")?;
        let report = std::path::PathBuf::from(
            std::env::var_os("BUTTERPOLLO_TEST_RFI_REPORT")
                .context("set BUTTERPOLLO_TEST_RFI_REPORT")?,
        );
        let directory = report
            .parent()
            .context("report requires a parent directory")?;
        let format = match codec {
            0 => "h264",
            1 => "hevc",
            _ => "obu",
        };
        let path = directory.join(format!("amf-rfi-{}-{codec}.{format}", std::process::id()));
        let bitstream: Vec<_> = output
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                !supported || !((4..8).contains(index) || (16..20).contains(index))
            })
            .flat_map(|(_, packet)| packet.bytes.iter().copied())
            .collect();
        std::fs::write(&path, bitstream)?;
        std::fs::write(
            path.with_extension(format!("full.{format}")),
            output
                .iter()
                .flat_map(|packet| packet.bytes.iter().copied())
                .collect::<Vec<_>>(),
        )?;
        eprintln!(
            "codec={codec}, supported={supported}, fixture={}",
            path.display()
        );
        let decoded_output = std::process::Command::new(&decoder)
            .args([
                "-hide_banner",
                "-nostdin",
                "-v",
                "error",
                "-xerror",
                "-err_detect",
                "explode",
                "-f",
                format,
                "-i",
            ])
            .arg(&path)
            .args([
                "-an",
                "-progress",
                "pipe:1",
                "-nostats",
                "-fps_mode",
                "passthrough",
                "-f",
                "null",
                "-",
            ])
            .creation_flags(0x08000000)
            .output()?;
        let stderr = String::from_utf8_lossy(&decoded_output.stderr);
        assert!(
            decoded_output.status.success() && stderr.is_empty(),
            "codec={codec}, {stderr}"
        );
        let decoded = String::from_utf8_lossy(&decoded_output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("frame=")?.trim().parse::<u32>().ok())
            .next_back()
            .context("decoder reported no frames")?;
        assert_eq!(decoded, if supported { 56 } else { 64 });
        reports.push(serde_json::json!({"codec":codec,"frames":output.len(),"decoded":decoded,"ltr_supported":supported,"recovery_frames":output.iter().filter(|frame|frame.after_invalidation).count(),"wrap_idr_fallback":supported&&codec==0,"dropped_frames":if supported{8}else{0}}));
    }
    if let Some(path) = std::env::var_os("BUTTERPOLLO_TEST_RFI_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&reports)?)?;
    }
    eprintln!("{}", serde_json::to_string(&reports)?);
    Ok(())
}
