//! How soon the capture sees a new Desktop Duplication frame: the time from
//! the frame's present (LastPresentTime) to the moment acquisition returns
//! it, polling as the host does or blocking in AcquireNextFrame, with the
//! process CPU time each costs. Not packaged.
//! usage: ddx_arrival_probe DISPLAY SECONDS poll|block [POLL_US]
//! With DDX_WAIT_LOCK set, another thread times D3D11 calls on the same
//! device meanwhile: a blocked acquisition holds the immediate context lock.
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows.");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};
    use windows::{
        Win32::{
            Foundation::FILETIME,
            Graphics::Dxgi::{Common::*, *},
            System::{
                Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
                Threading::{GetCurrentProcess, GetProcessTimes},
            },
        },
        core::Interface,
    };
    butterpollo_windows::capture::enable_dpi_awareness();
    let mut args = std::env::args().skip(1);
    let name = args.next().unwrap_or_default();
    let seconds: u64 = args.next().map_or(Ok(10), |s| s.parse())?;
    let block = args.next().as_deref() == Some("block");
    let poll = Duration::from_micros(args.next().map_or(Ok(500), |s| s.parse())?);
    let _priority = butterpollo_windows::capture::Priority::new();
    let gpu = butterpollo_windows::capture::Device::new(&name)?;
    let timer = butterpollo_windows::timing::Timer::new()?;
    let cpu = || unsafe {
        let (mut a, mut b, mut kernel, mut user) = Default::default();
        let _ = GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut kernel, &mut user);
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        (ticks(kernel) + ticks(user)) as f64 / 10_000.
    };
    unsafe {
        let mut frequency = 0;
        QueryPerformanceFrequency(&mut frequency)?;
        let output = gpu.output()?.cast::<IDXGIOutput5>()?;
        let duplicate = output.DuplicateOutput1(
            &gpu.device,
            0,
            &[DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_B8G8R8A8_UNORM],
        )?;
        let mut latencies = Vec::new();
        let mut frames = 0u64;
        let mut held = false;
        let started = Instant::now();
        let cpu_start = cpu();
        // `lock`: does a blocked acquisition hold the device's multithread
        // lock? Another thread times D3D11 calls on the same device meanwhile.
        let lock_probe = std::env::var_os("DDX_WAIT_LOCK").is_some().then(|| {
            let device = gpu.device.clone();
            let context = gpu.context.clone();
            let output = gpu.output().unwrap().cast::<IDXGIOutput6>().unwrap();
            let mut texture = None;
            device
                .CreateTexture2D(
                    &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC {
                        Width: 64,
                        Height: 64,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE_DEFAULT,
                        BindFlags: windows::Win32::Graphics::Direct3D11::D3D11_BIND_SHADER_RESOURCE
                            .0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut texture),
                )
                .unwrap();
            let texture = texture.unwrap();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = stop.clone();
            struct Send<T>(T);
            unsafe impl<T> std::marker::Send for Send<T> {}
            let objects = Send((device, context, output, texture));
            let thread = std::thread::spawn(move || {
                let objects = objects;
                let (_device, context, output, texture) = &objects.0;
                let mut calls = [Vec::new(), Vec::new(), Vec::new()];
                while !flag.load(std::sync::atomic::Ordering::Acquire) {
                    let at = Instant::now();
                    context.Flush();
                    calls[0].push(at.elapsed().as_secs_f64() * 1000.);
                    let at = Instant::now();
                    let mut desc = Default::default();
                    texture.GetDesc(&mut desc);
                    calls[1].push(at.elapsed().as_secs_f64() * 1000.);
                    let at = Instant::now();
                    let _ = output.GetDesc1();
                    calls[2].push(at.elapsed().as_secs_f64() * 1000.);
                    std::thread::sleep(Duration::from_millis(1));
                }
                calls
            });
            (stop, thread)
        });
        while started.elapsed() < Duration::from_secs(seconds) {
            if held {
                duplicate.ReleaseFrame()?;
                held = false;
            }
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource = None;
            match duplicate.AcquireNextFrame(if block { 100 } else { 0 }, &mut info, &mut resource)
            {
                Ok(()) => {
                    held = true;
                    frames += 1;
                    let mut now = 0;
                    QueryPerformanceCounter(&mut now)?;
                    if info.LastPresentTime != 0 {
                        latencies
                            .push((now - info.LastPresentTime) as f64 * 1000. / frequency as f64);
                    }
                }
                Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    if !block {
                        timer.until(Instant::now() + poll);
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        let cpu_ms = cpu() - cpu_start;
        if let Some((stop, thread)) = lock_probe {
            stop.store(true, std::sync::atomic::Ordering::Release);
            for (name, mut calls) in ["context Flush", "texture GetDesc", "output GetDesc1"]
                .into_iter()
                .zip(thread.join().unwrap())
            {
                calls.sort_by(f64::total_cmp);
                println!(
                    "other thread's {name} meanwhile: n {} p50 {:.3} p99 {:.3} max {:.3} ms",
                    calls.len(),
                    calls[calls.len() / 2],
                    calls[(calls.len() - 1) * 99 / 100],
                    calls.last().copied().unwrap_or(0.)
                );
            }
        }
        latencies.sort_by(f64::total_cmp);
        let at = |q: f64| {
            latencies
                .get((latencies.len().saturating_sub(1) as f64 * q) as usize)
                .copied()
                .unwrap_or(f64::NAN)
        };
        println!(
            "{} {}: frames {} present_samples {} detect mean {:.3} p50 {:.3} p95 {:.3} p99 {:.3} max {:.3} ms, cpu {:.1}% of a core",
            if block { "block" } else { "poll" },
            if block {
                String::new()
            } else {
                format!("{} us", poll.as_micros())
            },
            frames,
            latencies.len(),
            latencies.iter().sum::<f64>() / latencies.len() as f64,
            at(0.5),
            at(0.95),
            at(0.99),
            latencies.last().copied().unwrap_or(f64::NAN),
            cpu_ms / started.elapsed().as_secs_f64() / 10.
        );
    }
    Ok(())
}
