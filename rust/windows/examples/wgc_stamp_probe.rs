//! Is WGC's SystemRelativeTime in the past when the frame reaches the pool?
//! Polls TryGetNextFrame every 100 us and prints arrival minus stamp in ms;
//! negative values are stamps that lie in the future at arrival. Not packaged.
//! usage: wgc_stamp_probe DISPLAY SECONDS
//! DISPLAY is `\\.\DISPLAYn` (empty for primary); allow more than the one-second
//! warmup and keep the desktop moving. Run as the signed-in user with no stream.
//! Build with `cargo build -p butterpollo-windows --example wgc_stamp_probe
//! --release --target-dir target/qa` in the Rust SDK environment, with the
//! PyroWave runtime DLL beside the executable.
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
            Capture::{Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem},
            DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        },
        Win32::{
            Graphics::Dxgi::IDXGIDevice,
            System::{
                Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
                WinRT::{
                    Direct3D11::CreateDirect3D11DeviceFromDXGIDevice,
                    Graphics::Capture::IGraphicsCaptureItemInterop,
                },
            },
        },
        core::Interface,
    };
    let mut args = std::env::args().skip(1);
    let display = args.next().unwrap_or_default();
    let seconds: u64 = args.next().context("seconds required")?.parse()?;
    enable_dpi_awareness();
    let _com = ComGuard::new()?;
    let gpu = Device::new(&display)?;
    let timer = Timer::new()?;
    let mut frequency = 0;
    unsafe { QueryPerformanceFrequency(&mut frequency)? };
    let (pool, session) = unsafe {
        let desc = gpu.output()?.GetDesc()?;
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = interop.CreateForMonitor(desc.Monitor)?;
        let dxgi: IDXGIDevice = gpu.device.cast()?;
        let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            item.Size()?,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        session.SetIsBorderRequired(false)?;
        session.SetMinUpdateInterval(TimeSpan { Duration: 0 })?;
        session.StartCapture()?;
        (pool, session)
    };
    let mut deltas = Vec::new();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        let frame = unsafe {
            let mut raw = std::ptr::null_mut();
            (pool.vtable().TryGetNextFrame)(pool.as_raw(), &mut raw).ok()?;
            (!raw.is_null()).then(|| Direct3D11CaptureFrame::from_raw(raw))
        };
        if let Some(frame) = frame {
            let mut now = 0;
            unsafe { QueryPerformanceCounter(&mut now)? };
            let stamp =
                frame.SystemRelativeTime()?.Duration as i128 * frequency as i128 / 10_000_000;
            if start.elapsed() >= Duration::from_secs(1) {
                deltas.push((now as i128 - stamp) as f64 * 1000. / frequency as f64);
            }
            frame.Close()?;
        } else {
            timer.until(Instant::now() + Duration::from_micros(100));
        }
    }
    let _ = session.Close();
    let _ = pool.Close();
    ensure!(
        !deltas.is_empty(),
        "no WGC frames after the one-second warmup"
    );
    deltas.sort_by(f64::total_cmp);
    let at = |q: f64| deltas[((deltas.len() - 1) as f64 * q).round() as usize];
    let future = deltas.iter().filter(|d| **d < 0.).count();
    println!(
        "frames {} arrival-stamp ms: mean {:.3} p05 {:.3} p50 {:.3} p95 {:.3}; stamps in the future at arrival: {} ({:.1}%)",
        deltas.len(),
        deltas.iter().sum::<f64>() / deltas.len() as f64,
        at(0.05),
        at(0.5),
        at(0.95),
        future,
        future as f64 * 100. / deltas.len() as f64
    );
    Ok(())
}
