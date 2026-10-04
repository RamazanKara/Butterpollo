//! Repeat-frame encoder throughput excludes capture, network, and decoding.
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows.");
}
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::{Context, bail};
    use butterpollo_core::rtsp::Negotiated;
    use butterpollo_windows::{
        capture::{Capture, ComGuard, Priority},
        encoder::Encoder,
        timing::Timer,
    };
    use serde_json::json;
    use std::{
        collections::BTreeMap,
        time::{Duration, Instant},
    };
    let mut fields = BTreeMap::new();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args().skip(1);
    while let Some(key) = args.next() {
        if matches!(key.as_str(), "--help" | "-h") {
            println!(
                "Butterpollo encoder performance probe\n\
Repeat-frame throughput excludes capture, network, decoding and display latency.\n\
--width 1920 --height 1080 --fps 120 --seconds 8 --bitrate 20000\n\
--codec hevc (h264/hevc/av1/pyrowave) --encoder auto --capture wgc --display NAME\n\
--hdr: HDR10 output; --sdr-10bit; --yuv444; --records: PyroWave record framing; --cpu: CPU conversion/readback path; --paced: requested frame cadence; --live-capture: keep the shared capture device active; --arrival: with --live-capture, encode each new picture as it arrives"
            );
            return Ok(());
        }
        if matches!(
            key.as_str(),
            "--hdr"
                | "--sdr-10bit"
                | "--yuv444"
                | "--records"
                | "--cpu"
                | "--paced"
                | "--arrival"
                | "--spin"
                | "--priority"
                | "--live-capture"
        ) {
            fields.insert(key, "1".into());
        } else if matches!(
            key.as_str(),
            "--width"
                | "--height"
                | "--fps"
                | "--seconds"
                | "--bitrate"
                | "--codec"
                | "--encoder"
                | "--display"
                | "--capture"
                | "--config"
                | "--synthetic"
        ) {
            fields.insert(key, args.next().context("option requires a value")?);
        } else {
            bail!("unknown option {key}");
        }
    }
    let option =
        |key: &str, default: &str| fields.get(key).cloned().unwrap_or_else(|| default.into());
    let number = |key: &str, default: &str| option(key, default).parse::<u32>();
    let rate = butterpollo_core::framegen::Rate::parse(&option("--fps", "120"))?;
    let config = Negotiated {
        width: number("--width", "1920")?,
        height: number("--height", "1080")?,
        fps: rate.rounded(),
        rate_millihz: rate.0,
        bitrate_kbps: number("--bitrate", "20000")?,
        codec: match option("--codec", "hevc").as_str() {
            "h264" => 0,
            "hevc" => 1,
            "av1" => 2,
            "pyrowave" => 3,
            other => bail!("unknown codec {other}"),
        },
        hdr: fields.contains_key("--hdr"),
        sdr_10bit: fields.contains_key("--sdr-10bit"),
        yuv444: fields.contains_key("--yuv444"),
        pyrowave_records: fields.contains_key("--records"),
        ..Default::default()
    };
    let seconds = number("--seconds", "8")?;
    if !(2..=8192).contains(&config.width)
        || !(2..=8192).contains(&config.height)
        || !config.width.is_multiple_of(2)
        || !config.height.is_multiple_of(2)
        || !(1..=240).contains(&config.fps)
        || !(1..=300).contains(&seconds)
        || !(100..=if config.codec == 3 { 2_000_000 } else { 500000 })
            .contains(&config.bitrate_kbps)
    {
        bail!("probe settings outside supported bounds");
    }
    let _com = ComGuard::new()?;
    let _priority = Priority::new();
    let tuning = if let Some(path) = fields.get("--config") {
        butterpollo_core::config::Config::load(std::path::Path::new(path))?
    } else {
        Default::default()
    };
    // --synthetic N: N different moving pictures at the stream size instead
    // of a captured desktop, so every frame has new detail and motion.
    let synthetic: Vec<butterpollo_windows::capture::GpuImage> =
        match fields.get("--synthetic").map(|n| n.parse::<usize>()) {
            Some(count) => {
                let count = count?.clamp(2, 64);
                let device = butterpollo_windows::capture::Device::new(&option("--display", ""))?;
                // As a stream's capture device is configured.
                if fields.contains_key("--priority") {
                    butterpollo_windows::gpu_priority::configure(&device, &tuning)?;
                }
                (0..count)
                    .map(|frame| {
                        butterpollo_windows::capture::GpuImage::upload(
                            &device,
                            &moving_picture(config.width, config.height, frame),
                        )
                    })
                    .collect::<anyhow::Result<_>>()?
            }
            None => vec![],
        };
    let mut capture = if synthetic.is_empty() {
        Some(Capture::new_options(
            &option("--display", ""),
            &option("--capture", "wgc"),
            config.hdr,
            &tuning,
        )?)
    } else {
        None
    };
    let capture_backend = capture.as_ref().map_or("synthetic", |c| c.backend());
    let timeout = Instant::now() + Duration::from_secs(10);
    let image = match capture.as_mut() {
        None => synthetic[0].clone(),
        Some(capture) => loop {
            if let Some(image) = capture.next_gpu()? {
                break image;
            }
            if Instant::now() >= timeout {
                bail!("no capture frame within ten seconds");
            }
            std::thread::sleep(Duration::from_millis(1));
        },
    };
    let cpu = fields.contains_key("--cpu");
    let paced = fields.contains_key("--paced");
    let arrival = fields.contains_key("--arrival") && fields.contains_key("--live-capture");
    let spin = fields.contains_key("--spin");
    let latest = std::sync::Arc::new(std::sync::Mutex::new(image.clone()));
    let capture_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let live_capture_requested = fields.contains_key("--live-capture");
    let live_capture = if live_capture_requested && let Some(mut capture) = capture {
        let latest = latest.clone();
        let stop = capture_stop.clone();
        Some(std::thread::spawn(move || -> anyhow::Result<()> {
            let _com = ComGuard::new()?;
            let _priority = Priority::new();
            let timer = Timer::new()?;
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                if let Some(image) = capture.next_gpu()? {
                    *latest.lock().unwrap() = image;
                } else {
                    timer.until(Instant::now() + Duration::from_millis(1));
                }
            }
            Ok(())
        }))
    } else {
        None
    };
    let mut staging = None;
    let readback = if cpu {
        Some(image.readback(&mut staging)?)
    } else {
        None
    };
    let preference = option("--encoder", "auto");
    let mut encoder = if cpu {
        Encoder::new_options(
            &config,
            &preference,
            &image.gpu.display.display_name,
            &tuning,
        )?
    } else {
        Encoder::new_gpu_options(&config, &preference, &image, &tuning)?
    };
    let mut picture = 0usize;
    let mut encode = |encoder: &mut Encoder, idr| {
        if let Some(image) = readback.as_ref() {
            encoder.encode(image, idr, config.bitrate_kbps)
        } else if !synthetic.is_empty() {
            picture += 1;
            let mut image = synthetic[picture % synthetic.len()].clone();
            image.captured = Instant::now();
            encoder.encode_gpu(&image, idr, config.bitrate_kbps)
        } else {
            let image = latest.lock().unwrap().clone();
            encoder.encode_gpu(&image, idr, config.bitrate_kbps)
        }
    };
    for frame in 0..20 {
        encode(&mut encoder, frame == 0)?;
    }
    let timer = Timer::new()?;
    let warmup_deadline = Instant::now() + Duration::from_secs(2);
    while encoder.pending() {
        encoder.poll()?;
        if Instant::now() >= warmup_deadline {
            bail!("encoder did not complete warmup frames");
        }
        timer.until(Instant::now() + Duration::from_micros(250));
    }
    let start = Instant::now();
    let end = start + Duration::from_secs(seconds.into());
    let period = rate.period();
    let mut due = start;
    let (mut frames, mut bytes, mut submits) = (0u64, 0u64, 0u64);
    let (mut calls, mut latencies) = (Vec::new(), Vec::new());
    // Live capture: from DWM presenting a picture to its bitstream, counted
    // once per presented picture (the paced loop may encode one twice).
    let (mut present, mut last_presented) = (Vec::new(), None);
    let mut consume = |output: Vec<butterpollo_windows::encoder::Encoded>| {
        for frame in output {
            frames += 1;
            bytes += frame.bytes.len() as u64;
            if let Some(latency) = frame.latency {
                latencies.push(latency.as_secs_f64() * 1000.);
            }
            if let Some(presented) = frame.presentation
                && last_presented != Some(presented)
            {
                last_presented = Some(presented);
                present.push(presented.elapsed().as_secs_f64() * 1000.);
            }
        }
    };
    let mut encoded_presentation = None;
    while Instant::now() < end {
        if arrival {
            // Encode each captured picture as soon as it arrives, as a
            // stream with arrival pacing does.
            let presented = latest.lock().unwrap().captured;
            if encoded_presentation == Some(presented) {
                if encoder.pending() {
                    consume(encoder.poll()?);
                }
                timer.until(Instant::now() + Duration::from_micros(100));
                continue;
            }
            encoded_presentation = Some(presented);
        } else if paced {
            while Instant::now() < due {
                if encoder.pending() {
                    consume(encoder.poll()?);
                    if encoder.pending() && !spin {
                        timer.until((Instant::now() + Duration::from_micros(250)).min(due));
                    }
                } else {
                    timer.until(due);
                }
            }
            due += period;
            due = due.max(Instant::now());
        }
        let tick = Instant::now();
        consume(encode(&mut encoder, false)?);
        calls.push(tick.elapsed().as_secs_f64() * 1000.);
        submits += 1;
    }
    let elapsed = start.elapsed().as_secs_f64();
    capture_stop.store(true, std::sync::atomic::Ordering::Release);
    if let Some(worker) = live_capture {
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("capture probe panicked"))??;
    }
    fn stats(mut values: Vec<f64>) -> serde_json::Value {
        if values.is_empty() {
            return serde_json::Value::Null;
        }
        values.sort_by(f64::total_cmp);
        json!({"mean_ms":values.iter().sum::<f64>() / values.len() as f64,"p50_ms":values[(values.len()-1)/2],"p95_ms":values[(values.len()-1)*95/100],"samples":values.len()})
    }
    println!(
        "{}",
        json!({"scope":if fields.contains_key("--live-capture") { "live capture encoder probe; timing begins at encoder submission, excludes capture age, network, decode and display latency" } else { "repeat-frame encoder throughput; excludes capture, network, decode and display latency" }, "capture_backend":capture_backend,"live_capture":fields.contains_key("--live-capture"),"capture_width":image.width,"capture_height":image.height,"adapter":image.gpu.display.adapter,"width":config.width,"height":config.height,"fps":config.fps,"codec":config.codec,"hdr":config.hdr,"cpu":cpu,"paced":paced,"seconds":elapsed,"submits":submits,"completed_frames":frames,"encoded_fps":frames as f64 / elapsed,"bytes":bytes,"encode_call":stats(calls),"submission_to_observed_output":stats(latencies),"present_to_output":if live_capture_requested { stats(present) } else { serde_json::Value::Null }})
    );
    Ok(())
}
/// A picture like a moving game scene: scrolling gradients, edges, and a
/// noisy patch that moves across the frame.
#[cfg(windows)]
fn moving_picture(width: u32, height: u32, frame: usize) -> butterpollo_windows::capture::Image {
    let (w, h) = (width as usize, height as usize);
    let mut bytes = vec![0u8; w * h * 4];
    let shift = frame * 24;
    let (patch_x, patch_y) = ((frame * 97) % w.max(1), (frame * 53) % h.max(1));
    let mut seed = 0x9e37_79b9_7f4a_7c15u64 ^ frame as u64;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let inside = x.wrapping_sub(patch_x) < w / 5 && y.wrapping_sub(patch_y) < h / 5;
            let (b, g, r) = if inside {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed as u8, (seed >> 8) as u8, (seed >> 16) as u8)
            } else {
                let stripe = if ((x + shift) / 64 + y / 64).is_multiple_of(2) {
                    40
                } else {
                    0
                };
                (
                    ((x + shift) % 256) as u8,
                    ((y + shift / 3) % 256) as u8,
                    (((x ^ y) + shift) % 216) as u8 + stripe,
                )
            };
            bytes[i..i + 4].copy_from_slice(&[b, g, r, 255]);
        }
    }
    butterpollo_windows::capture::Image {
        width,
        height,
        stride: w * 4,
        bytes,
        captured: std::time::Instant::now(),
        pixel: butterpollo_windows::capture::Pixel::Bgra8,
    }
}
