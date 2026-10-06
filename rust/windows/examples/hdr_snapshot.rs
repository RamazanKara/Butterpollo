//! One-frame, read-only HDR capture/encode diagnostic. Never run by the host.
//! hdr_snapshot EXPLICIT_DISPLAY NEW_OUTPUT_DIRECTORY [--helper]
//! No display, service, input, application, or installed configuration changes.
#[cfg(windows)]
mod support;
#[cfg(not(windows))]
fn main() {
    eprintln!("This diagnostic requires Windows.");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    // Only the owned helper child takes this path. It runs capture/transport,
    // never the parent snapshot, encoder, output files, or a full host.
    if let Some(result) = support::wgc_worker() {
        return result;
    }
    use anyhow::{Context, ensure};
    use butterpollo_core::{config::Config, rtsp::Negotiated};
    use butterpollo_windows::{
        amf,
        capture::{Capture, ComGuard, Pixel, enable_dpi_awareness},
        compute::Compute,
        display::monitors,
    };
    use std::{
        io::Write,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, Instant},
    };

    // A blocked native API cannot evade the phase deadline. This kills only
    // this diagnostic process; Windows closes its private capture/GPU handles.
    struct Budget {
        started: Instant,
        deadline_ms: Arc<AtomicU64>,
    }
    impl Budget {
        fn new() -> Self {
            let started = Instant::now();
            let deadline_ms = Arc::new(AtomicU64::new(10_000));
            let deadline = deadline_ms.clone();
            std::thread::spawn(move || {
                loop {
                    let limit = deadline.load(Ordering::Acquire);
                    if limit == 0 {
                        return;
                    }
                    if started.elapsed().as_millis() as u64 >= limit {
                        eprintln!("HDR snapshot native operation exceeded its phase deadline");
                        std::process::exit(124);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
            Self {
                started,
                deadline_ms,
            }
        }
        fn phase(&self, seconds: u64) {
            self.deadline_ms.store(
                self.started.elapsed().as_millis() as u64 + seconds * 1000,
                Ordering::Release,
            );
        }
    }
    impl Drop for Budget {
        fn drop(&mut self) {
            self.deadline_ms.store(0, Ordering::Release);
        }
    }

    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 2 || (args.len() == 3 && args[2] == "--helper"),
        "usage: hdr_snapshot EXPLICIT_DISPLAY NEW_OUTPUT_DIRECTORY [--helper]"
    );
    let helper = args.len() == 3;
    let capture_mode = if helper {
        "wgc_user_helper"
    } else {
        "wgc_direct"
    };
    let output = PathBuf::from(&args[1]);
    ensure!(output.is_absolute(), "output directory must be absolute");
    ensure!(!output.exists(), "output directory must be new");
    std::fs::create_dir(&output)?;
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();

    let budget = Budget::new();
    ensure!(
        !butterpollo_windows::process::is_system(),
        "run this read-only diagnostic in the existing interactive user session"
    );
    enable_dpi_awareness();
    let _com = ComGuard::new()?;
    let before = monitors()?;
    let selected = before
        .iter()
        .find(|m| m.display_name.eq_ignore_ascii_case(&args[0]))
        .context("explicit display is not active; no primary-display fallback")?;
    ensure!(
        selected.hdr_enabled,
        "selected display must already have HDR enabled"
    );
    // Use the production owned/shared texture handoff. Wgc::new_format alone
    // creates a plain D3D11 copy that a D3D12 encoder cannot open by NT handle.
    let mut options = Config::parse(
        "gpu_compute_conversion = true\nwgc_compute_copy = true\nwgc_user_helper = false\n",
    )?;
    options
        .values
        .insert("wgc_user_helper".into(), helper.to_string());
    let mut capture = Capture::new_options(&selected.display_name, "wgc", true, &options)?;
    ensure!(
        matches!(&capture, Capture::WgcWorker(_)) == helper,
        "selected capture mode was not used"
    );
    let limit = Instant::now() + Duration::from_secs(10);
    let image = loop {
        if let Some(image) = capture.next_gpu()? {
            break image;
        }
        ensure!(Instant::now() < limit, "no WGC image within ten seconds");
        std::thread::sleep(Duration::from_millis(1));
    };
    ensure!(
        image.gpu.display.display_name == selected.display_name,
        "capture resolved to a different display"
    );
    ensure!(
        image.pixel == Pixel::RgbaF16,
        "native FP16 capture required"
    );
    ensure!(
        butterpollo_windows::compute::shareable(&image.texture),
        "production WGC compute handoff did not produce a shared GPU image"
    );
    ensure!(
        (image.width, image.height) == (3840, 2160),
        "diagnostic requires the existing 3840x2160 desktop"
    );
    drop(capture);
    let captured_ms = budget.started.elapsed().as_millis();

    budget.phase(5);
    // Same owned GPU image is read here and submitted below. No recapture,
    // upload, gamut adjustment, SDR-white adjustment, or tone map is performed.
    let source = image.readback(&mut None)?;
    let readback_ms = budget.started.elapsed().as_millis();
    budget.phase(10);
    std::fs::write(output.join("source-rgba16f.raw"), &source.bytes)?;
    std::fs::write(
        output.join("source.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "display": selected,
            "width": source.width,
            "height": source.height,
            "stride": source.stride,
            "format": "RGBA little-endian IEEE754 binary16 / linear scRGB / 1.0 = 80 nits",
            "capture": "native WGC, one owned frame",
            "capture_mode": capture_mode,
            "capture_compute_handoff": true,
            "capture_elapsed_ms": captured_ms,
            "readback_elapsed_ms": readback_ms,
            "hdr_metadata": image.gpu.hdr_metadata(),
            "before": before,
        }))?,
    )?;
    drop(source);

    budget.phase(5);
    let config = Negotiated {
        width: 3840,
        height: 2160,
        fps: 120,
        rate_millihz: 120_000,
        bitrate_kbps: 80_000,
        codec: 1,
        hdr: true,
        csc_mode: 4,
        ..Default::default()
    };
    let compute = Compute::for_device(&image.gpu.device)?;
    let mut encoder = amf::Encoder::new_gpu(&config, image.gpu.clone(), &options, Some(compute))?;
    let metadata = image.gpu.hdr_metadata();
    encoder.set_hdr_metadata(metadata);
    let mut frames = encoder.encode_gpu(&image, true, config.bitrate_kbps)?;
    while frames.is_empty() {
        frames.extend(encoder.poll()?);
        if frames.is_empty() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    ensure!(
        frames.len() == 1 && frames[0].idr,
        "expected exactly one IDR"
    );
    let encoded = frames.remove(0);
    drop(encoder);
    drop(image);

    budget.phase(10);
    let mut stream = std::fs::File::create_new(output.join("snapshot.hevc"))?;
    stream.write_all(&encoded.bytes)?;
    stream.sync_all()?;
    let after = monitors()?;
    ensure!(
        serde_json::to_value(&before)? == serde_json::to_value(&after)?,
        "active monitor identities/HDR flags changed during observation"
    );
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "status": "captured_and_encoded",
            "encoded_frames": 1,
            "encoded_bytes": encoded.bytes.len(),
            "encoder": "native AMF / D3D12 compute / HEVC Main10",
            "capture_mode": capture_mode,
            "width": 3840, "height": 2160,
            "fps": 120, "bitrate_kbps": 80_000,
            "hdr": true, "color_matrix": 2, "full_range": false,
            "native_fp16_scale": 1.0,
            "hdr_metadata": metadata,
            "encoder_latency_ms": encoded.latency.map(|v| v.as_secs_f64() * 1000.0),
            "total_elapsed_ms": budget.started.elapsed().as_millis(),
            "monitor_identity_and_hdr_flags_unchanged": true,
            "after": after,
            "limits": "No client renderer or physical HDR output validated. One source image only. Independent decode required.",
        }))?,
    )?;
    Ok(())
}
