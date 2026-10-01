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
    let mut args = std::env::args().skip(1);
    while let Some(key) = args.next() {
        if matches!(key.as_str(), "--help" | "-h") {
            println!(
                "Butterpollo encoder performance probe\n\
Repeat-frame throughput excludes capture, network, decoding and display latency.\n\
--width 1920 --height 1080 --fps 120 --seconds 8 --bitrate 20000\n\
--codec hevc (h264/hevc/av1/pyrowave) --encoder auto --capture wgc --display NAME\n\
--hdr: HDR10 output; --cpu: CPU conversion/readback path; --paced: requested frame cadence"
            );
            return Ok(());
        }
        if matches!(key.as_str(), "--hdr" | "--cpu" | "--paced") {
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
        ..Default::default()
    };
    let seconds = number("--seconds", "8")?;
    if !(2..=8192).contains(&config.width)
        || !(2..=8192).contains(&config.height)
        || !config.width.is_multiple_of(2)
        || !config.height.is_multiple_of(2)
        || !(1..=240).contains(&config.fps)
        || !(1..=300).contains(&seconds)
        || !(100..=500000).contains(&config.bitrate_kbps)
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
    let mut capture = Capture::new_options(
        &option("--display", ""),
        &option("--capture", "wgc"),
        config.hdr,
        &tuning,
    )?;
    let timeout = Instant::now() + Duration::from_secs(10);
    let image = loop {
        if let Some(image) = capture.next_gpu()? {
            break image;
        }
        if Instant::now() >= timeout {
            bail!("no capture frame within ten seconds");
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let cpu = fields.contains_key("--cpu");
    let paced = fields.contains_key("--paced");
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
    let encode = |encoder: &mut Encoder, idr| {
        if let Some(image) = readback.as_ref() {
            encoder.encode(image, idr, config.bitrate_kbps)
        } else {
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
    let mut consume = |output: Vec<butterpollo_windows::encoder::Encoded>| {
        for frame in output {
            frames += 1;
            bytes += frame.bytes.len() as u64;
            if let Some(latency) = frame.latency {
                latencies.push(latency.as_secs_f64() * 1000.);
            }
        }
    };
    while Instant::now() < end {
        if paced {
            while Instant::now() < due {
                if encoder.pending() {
                    consume(encoder.poll()?);
                    if encoder.pending() {
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
    fn stats(mut values: Vec<f64>) -> serde_json::Value {
        if values.is_empty() {
            return serde_json::Value::Null;
        }
        values.sort_by(f64::total_cmp);
        json!({"mean_ms":values.iter().sum::<f64>() / values.len() as f64,"p50_ms":values[(values.len()-1)/2],"p95_ms":values[(values.len()-1)*95/100],"samples":values.len()})
    }
    println!(
        "{}",
        json!({"scope":"repeat-frame encoder throughput; excludes capture, network, decode and display latency", "capture_width":image.width,"capture_height":image.height,"adapter":image.gpu.display.adapter,"width":config.width,"height":config.height,"fps":config.fps,"codec":config.codec,"hdr":config.hdr,"cpu":cpu,"paced":paced,"seconds":elapsed,"submits":submits,"completed_frames":frames,"encoded_fps":frames as f64 / elapsed,"bytes":bytes,"encode_call":stats(calls),"submission_to_observed_output":stats(latencies)})
    );
    Ok(())
}
