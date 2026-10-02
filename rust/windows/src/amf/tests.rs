use super::*;

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
