//! Measure native WGC timestamps without copying, encoding or saving pictures.
//! Changes only this probe's capture interval, never the display or game.
//! usage: wgc_cadence_probe DISPLAY SECONDS_PER_INTERVAL OUTPUT_JSON
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows.");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use butterpollo_windows::{
        capture::{ComGuard, Device, enable_dpi_awareness},
        timing::Timer,
    };
    use std::time::{Duration, Instant};
    use windows::{
        Foundation::TimeSpan,
        Graphics::{
            Capture::{
                Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
                GraphicsCaptureSession,
            },
            DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        },
        Win32::{
            Foundation::HMODULE,
            Graphics::Dxgi::IDXGIDevice,
            System::{
                LibraryLoader::{GET_MODULE_HANDLE_EX_FLAG_PIN, GetModuleHandleExW},
                WinRT::{
                    Direct3D11::CreateDirect3D11DeviceFromDXGIDevice,
                    Graphics::Capture::IGraphicsCaptureItemInterop,
                },
            },
        },
        core::Interface,
    };
    struct Capture {
        pool: Direct3D11CaptureFramePool,
        session: GraphicsCaptureSession,
    }
    impl Drop for Capture {
        fn drop(&mut self) {
            let _ = self.session.Close();
            let _ = self.pool.Close();
        }
    }
    struct Frame(Direct3D11CaptureFrame);
    impl Drop for Frame {
        fn drop(&mut self) {
            let _ = self.0.Close();
        }
    }
    let mut args = std::env::args().skip(1);
    let display = args.next().unwrap_or_default();
    let seconds: u64 = args.next().context("seconds required")?.parse()?;
    ensure!((3..=10).contains(&seconds), "seconds must be 3..10");
    let report = args.next().context("output path required")?;
    enable_dpi_awareness();
    let _com = ComGuard::new()?;
    let gpu = Device::new(&display)?;
    let timer = Timer::new()?;
    let open_capture = |interval_us: i64| -> anyhow::Result<Capture> {
        unsafe {
            let desc = gpu.output()?.GetDesc()?;
            let interop: IGraphicsCaptureItemInterop =
                windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            let item: GraphicsCaptureItem = interop.CreateForMonitor(desc.Monitor)?;
            let mut module = HMODULE::default();
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_PIN,
                windows::core::w!("GraphicsCapture.dll"),
                &mut module,
            )?;
            let dxgi: IDXGIDevice = gpu.device.cast()?;
            let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                &winrt,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                2,
                item.Size()?,
            )?;
            let session = pool.CreateCaptureSession(&item)?;
            session.SetIsCursorCaptureEnabled(true)?;
            // Abort if suppressing the capture border is unavailable: this probe
            // must not introduce a visible overlay during a game.
            session.SetIsBorderRequired(false)?;
            if interval_us >= 0 {
                session.SetMinUpdateInterval(TimeSpan {
                    Duration: interval_us * 10,
                })?;
            }
            let capture = Capture { pool, session };
            capture.session.StartCapture()?;
            Ok(capture)
        }
    };
    let mut results = Vec::new();
    for interval_us in [-1, 0, 1000, -1] {
        let capture = open_capture(interval_us)?;
        let start = Instant::now();
        let mut frames = Vec::new();
        let mut empty = 0u64;
        while start.elapsed() < Duration::from_secs(seconds) {
            let frame = unsafe {
                let mut raw = std::ptr::null_mut();
                (capture.pool.vtable().TryGetNextFrame)(capture.pool.as_raw(), &mut raw).ok()?;
                (!raw.is_null()).then(|| Frame(Direct3D11CaptureFrame::from_raw(raw)))
            };
            if let Some(frame) = frame {
                if start.elapsed() >= Duration::from_secs(1) {
                    frames.push(serde_json::json!({
                        "timestamp_100ns": frame.0.SystemRelativeTime()?.Duration,
                        "observed_us": start.elapsed().as_micros() as u64,
                        "dirty_regions": frame.0.DirtyRegions().and_then(|r| r.Size()).ok(),
                    }));
                }
            } else {
                empty += 1;
                timer.until(Instant::now() + Duration::from_micros(500));
            }
        }
        let timestamps: Vec<i64> = frames
            .iter()
            .filter_map(|f| f["timestamp_100ns"].as_i64())
            .collect();
        let mut gaps: Vec<f64> = timestamps
            .windows(2)
            .map(|p| (p[1] - p[0]) as f64 / 10_000.)
            .collect();
        gaps.sort_by(f64::total_cmp);
        let fps = (timestamps.len() > 1).then(|| {
            (timestamps.len() - 1) as f64 * 10_000_000.
                / (timestamps.last().unwrap() - timestamps[0]) as f64
        });
        let summary = serde_json::json!({"interval_us":interval_us,"reported_interval_100ns":capture.session.MinUpdateInterval().ok().map(|v|v.Duration),"frames":frames.len(),"fps":fps,
            "gap_p50_ms":gaps.get(gaps.len()/2),"gap_p95_ms":gaps.get(gaps.len().saturating_sub(1)*95/100),
            "empty_checks":empty});
        println!("{summary}");
        results.push(serde_json::json!({"summary":summary,"samples":frames}));
    }
    std::fs::write(
        report,
        serde_json::to_vec_pretty(&serde_json::json!({
            "scope":"Native WGC timestamps only; no pixel copy, encoder, network, saved pictures, input or display changes. Does not establish distinct-picture FPS.",
            "display":gpu.display,"results":results,
        }))?,
    )?;
    Ok(())
}
