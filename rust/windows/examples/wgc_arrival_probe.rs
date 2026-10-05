//! Compare WGC frame detection with polling and FrameArrived notifications.
//! Captures the selected desktop without saving pictures or changing displays.
//! usage: wgc_arrival_probe DISPLAY SECONDS poll|notify|hybrid [POLL_US]
#[cfg(windows)]
mod support;
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows.");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    if let Some(result) = support::wgc_worker() {
        return result;
    }
    use anyhow::ensure;
    use butterpollo_windows::{
        capture::{Capture, ComGuard, Priority, enable_dpi_awareness},
        timing::{StreamingScope, Timer},
    };
    use std::time::{Duration, Instant};
    use windows::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentProcess, GetProcessTimes},
    };

    let mut args = std::env::args().skip(1);
    let display = args.next().unwrap_or_default();
    let seconds: u64 = args.next().map_or(Ok(10), |s| s.parse())?;
    let mode = args.next().unwrap_or_else(|| "notify".into());
    let poll_us: u64 = args.next().map_or(Ok(500), |s| s.parse())?;
    ensure!((1..=300).contains(&seconds), "seconds must be 1..300");
    ensure!(
        matches!(mode.as_str(), "poll" | "notify" | "hybrid"),
        "expected poll, notify or hybrid"
    );
    ensure!(
        (100..=1000).contains(&poll_us),
        "poll interval must be 100..1000 us"
    );
    enable_dpi_awareness();
    let _com = ComGuard::new()?;
    let _scope = StreamingScope::enter();
    let _priority = Priority::new();
    let timer = Timer::new()?;
    // Strict open: this probe must fail if WGC is unavailable, never measure DDX.
    let mut capture = Capture::new(&display, "wgc")?;
    capture.enable_frame_notifications()?;
    let cpu = || -> anyhow::Result<f64> {
        let (mut created, mut exited, mut kernel, mut user) = Default::default();
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )?;
        }
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        Ok((ticks(kernel) + ticks(user)) as f64 / 10_000.)
    };
    let mut ages = Vec::new();
    let mut empty = 0u64;
    let mut actual_display = None;
    let start = Instant::now();
    let end = start + Duration::from_secs(seconds);
    let cpu_start = cpu()?;
    while Instant::now() < end {
        if let Some(image) = capture.next_gpu()? {
            actual_display.get_or_insert_with(|| image.gpu.display.display_name.clone());
            // Leave startup out: WGC can deliver an old initial desktop.
            if start.elapsed() >= Duration::from_secs(1) {
                ages.push(
                    image
                        .acquired
                        .saturating_duration_since(image.captured)
                        .as_secs_f64()
                        * 1000.,
                );
            }
        } else {
            empty += 1;
            if mode != "poll" {
                let interval = if mode == "hybrid" {
                    Duration::from_micros(poll_us)
                } else {
                    Duration::from_millis(100)
                };
                capture.wait_until(&timer, (Instant::now() + interval).min(end))?;
            } else {
                timer.until((Instant::now() + Duration::from_micros(poll_us)).min(end));
            }
        }
    }
    let cpu_ms = cpu()? - cpu_start;
    let seconds = start.elapsed().as_secs_f64();
    ensure!(actual_display.is_some(), "WGC delivered no frames");
    ages.sort_by(f64::total_cmp);
    let detection = (!ages.is_empty()).then(|| {
        serde_json::json!({
            "mean_ms": ages.iter().sum::<f64>() / ages.len() as f64,
            "p50_ms": ages[(ages.len() - 1) / 2],
            "p95_ms": ages[(ages.len() - 1) * 95 / 100],
            "p99_ms": ages[(ages.len() - 1) * 99 / 100],
        })
    });
    println!(
        "{}",
        serde_json::json!({
            "scope": "WGC timestamp to capture detection; excludes encoding, transport and decoding",
            "display": actual_display, "backend": capture.backend(), "mode": mode,
            "seconds": seconds, "poll_us": poll_us, "samples": ages.len(),
            "empty_checks": empty, "process_cpu_ms": cpu_ms,
            "process_cpu_percent_of_one_core": cpu_ms / (seconds * 10.),
            "detection": detection,
        })
    );
    Ok(())
}
