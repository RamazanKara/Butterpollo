use super::*;
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
