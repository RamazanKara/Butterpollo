//! Read-only DXGI wait timing. No readback, display changes or test windows.
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows.");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use butterpollo_windows::capture::{ComGuard, Device};
    use std::time::{Duration, Instant};
    use windows::Win32::Graphics::Dxgi::{DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO};

    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let duplication = unsafe { gpu.output.DuplicateOutput(&gpu.device)? };
    let timer = butterpollo_windows::timing::Timer::new()?;
    let mut cases = vec![];
    for timeout_ms in [0, 1, 2, 4] {
        let mut waits = vec![];
        let mut arrivals = 0;
        let deadline = Instant::now() + Duration::from_secs(4);
        while waits.len() < 40 && Instant::now() < deadline {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource = None;
            let start = Instant::now();
            let result =
                unsafe { duplication.AcquireNextFrame(timeout_ms, &mut info, &mut resource) };
            let elapsed = start.elapsed().as_secs_f64() * 1000.;
            match result {
                Ok(()) => {
                    arrivals += 1;
                    drop(resource);
                    unsafe {
                        duplication.ReleaseFrame()?;
                    }
                }
                Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => waits.push(elapsed),
                Err(error) => return Err(error.into()),
            }
            // Yield the device between trials, including after a driver timeout.
            timer.until(Instant::now() + Duration::from_millis(1));
        }
        waits.sort_by(f64::total_cmp);
        anyhow::ensure!(!waits.is_empty(), "No timeout samples on this desktop");
        cases.push(serde_json::json!({
            "requested_timeout_ms":timeout_ms,
            "samples":waits.len(),
            "arrivals_excluded":arrivals,
            "mean_ms":waits.iter().sum::<f64>()/waits.len() as f64,
            "p95_ms":waits[(waits.len()-1)*95/100],
            "p99_ms":waits[(waits.len()-1)*99/100],
            "max_ms":waits.last(),
        }));
    }
    println!(
        "{}",
        serde_json::json!({
            "scope":"AcquireNextFrame timeout on a separate read-only device; excludes GPU copies, shared-device contention, encoding and delivery",
            "display":gpu.display,
            "cases":cases,
        })
    );
    Ok(())
}
