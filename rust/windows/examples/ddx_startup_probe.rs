//! Diagnose capture startup without changing the display mode or sending input.
//! Usage: ddx_startup_probe DISPLAY SECONDS raw-bgra|raw-fp16|raw-legacy|plain|graphics|compute|wgc [awake|passive] [follow|stay]
//! JSON lines contain counts and sampled pixel ranges, never desktop images.
//! Readback makes this a correctness diagnostic, not a latency benchmark.
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows.");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    probe::run()
}

#[cfg(windows)]
mod probe {
    use anyhow::{Context, Result, bail, ensure};
    use butterpollo_core::config::Config;
    use butterpollo_windows::{
        capture::{Capture, ComGuard, Device, Duplication, GpuImage, Pixel, Priority},
        timing::{DisplayAwake, Timer},
    };
    use serde_json::json;
    use std::time::{Duration, Instant};
    use windows::{
        Win32::Graphics::{
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
        core::Interface,
    };

    enum Source {
        Raw {
            gpu: Device,
            duplication: IDXGIOutputDuplication,
            held: bool,
        },
        Plain(Box<Duplication>),
        Configured(Capture),
    }

    impl Source {
        fn open(name: &str, mode: &str) -> Result<Self> {
            if mode.starts_with("raw-") {
                let gpu = Device::new(name)?;
                let output = gpu.output()?;
                let duplication = unsafe {
                    if mode == "raw-legacy" {
                        output.DuplicateOutput(&gpu.device)?
                    } else {
                        let formats = match mode {
                            "raw-bgra" => {
                                [DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT]
                            }
                            "raw-fp16" => {
                                [DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_B8G8R8A8_UNORM]
                            }
                            _ => bail!("unknown mode {mode}"),
                        };
                        output
                            .cast::<IDXGIOutput5>()?
                            .DuplicateOutput1(&gpu.device, 0, &formats)?
                    }
                };
                let desc = unsafe { duplication.GetDesc() };
                println!(
                    "{}",
                    json!({"event":"opened_raw", "display":gpu.display,
                    "format":desc.ModeDesc.Format.0, "width":desc.ModeDesc.Width,
                    "height":desc.ModeDesc.Height, "system_memory":desc.DesktopImageInSystemMemory.as_bool()})
                );
                Ok(Self::Raw {
                    gpu,
                    duplication,
                    held: false,
                })
            } else if mode == "plain" {
                Ok(Self::Plain(Box::new(Duplication::new(name)?)))
            } else {
                ensure!(
                    matches!(mode, "graphics" | "compute" | "wgc"),
                    "unknown mode {mode}"
                );
                let config = Config::parse(&format!(
                    "gpu_compute_conversion = {}\n",
                    mode != "graphics"
                ))?;
                Ok(Self::Configured(Capture::new_options(
                    name,
                    if mode == "wgc" { "wgc" } else { "ddx" },
                    false,
                    &config,
                )?))
            }
        }

        fn next(&mut self, detail: bool) -> Result<Option<serde_json::Value>> {
            match self {
                Self::Raw {
                    gpu,
                    duplication,
                    held,
                } => unsafe {
                    if std::mem::replace(held, false) {
                        duplication.ReleaseFrame()?;
                    }
                    let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                    let mut resource = None;
                    match duplication.AcquireNextFrame(0, &mut info, &mut resource) {
                        Ok(()) => *held = true,
                        Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
                        Err(error) => return Err(error.into()),
                    }
                    let texture: ID3D11Texture2D =
                        resource.context("empty capture resource")?.cast()?;
                    let mut desc = D3D11_TEXTURE2D_DESC::default();
                    texture.GetDesc(&mut desc);
                    let mut value = json!({"present_qpc":info.LastPresentTime,
                        "pointer_qpc":info.LastMouseUpdateTime, "accumulated":info.AccumulatedFrames,
                        "protected_masked":info.ProtectedContentMaskedOut.as_bool(),
                        "width":desc.Width,"height":desc.Height,"format":desc.Format.0});
                    if detail && desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM {
                        // Inspect a sparse RGB range only; alpha is not evidence of picture content.
                        desc.Usage = D3D11_USAGE_STAGING;
                        desc.BindFlags = 0;
                        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                        desc.MiscFlags = 0;
                        let mut staging = None;
                        gpu.device
                            .CreateTexture2D(&desc, None, Some(&mut staging))?;
                        let staging = staging.context("missing staging texture")?;
                        gpu.context.CopyResource(&staging, &texture);
                        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                        gpu.context
                            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                        let bytes = std::slice::from_raw_parts(
                            mapped.pData.cast::<u8>(),
                            mapped.RowPitch as usize * desc.Height as usize,
                        );
                        let range =
                            rgb_range(bytes, mapped.RowPitch as usize, desc.Width, desc.Height);
                        gpu.context.Unmap(&staging, 0);
                        value["rgb_range"] = json!(range);
                    }
                    Ok(Some(value))
                },
                Self::Plain(capture) => describe(capture.next_gpu()?, detail),
                Self::Configured(capture) => describe(capture.next_gpu()?, detail),
            }
        }
    }

    impl Drop for Source {
        fn drop(&mut self) {
            if let Self::Raw {
                duplication,
                held: true,
                ..
            } = self
            {
                unsafe {
                    let _ = duplication.ReleaseFrame();
                }
            }
        }
    }

    fn rgb_range(bytes: &[u8], stride: usize, width: u32, height: u32) -> [u8; 2] {
        let (mut low, mut high) = (255, 0);
        for row in 0..7 {
            for column in 0..11 {
                let y = (height - 1) as usize * row / 6;
                let x = (width - 1) as usize * column / 10;
                for &value in &bytes[y * stride + x * 4..y * stride + x * 4 + 3] {
                    low = low.min(value);
                    high = high.max(value);
                }
            }
        }
        [low, high]
    }

    fn describe(image: Option<GpuImage>, detail: bool) -> Result<Option<serde_json::Value>> {
        let Some(image) = image else { return Ok(None) };
        let mut value =
            json!({"width":image.width,"height":image.height,"pixel":format!("{:?}",image.pixel)});
        if detail {
            let cpu = image.readback(&mut None)?;
            if cpu.pixel == Pixel::Bgra8 {
                value["rgb_range"] =
                    json!(rgb_range(&cpu.bytes, cpu.stride, cpu.width, cpu.height));
            }
        }
        Ok(Some(value))
    }

    pub fn run() -> Result<()> {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .init();
        let mut args = std::env::args().skip(1);
        let name = args
            .next()
            .context("DISPLAY required (empty selects primary)")?;
        let seconds: u64 = args.next().context("SECONDS required")?.parse()?;
        ensure!((1..=600).contains(&seconds), "SECONDS must be 1..600");
        let mode = args.next().context("mode required")?;
        let awake = args.next().as_deref() != Some("passive");
        let follow = args.next().as_deref() != Some("stay");
        butterpollo_windows::capture::enable_dpi_awareness();
        let _com = ComGuard::new()?;
        let _priority = Priority::new();
        let followed = follow && butterpollo_windows::input::follow_input_desktop();
        let started = Instant::now();
        let _awake = awake.then(DisplayAwake::enter).transpose()?;
        let timer = Timer::new()?;
        println!(
            "{}",
            json!({"event":"start", "mode":mode, "awake":awake, "followed_input_desktop":followed})
        );
        let mut source = None;
        let (mut frames, mut empty, mut failures, mut opens) = (0u64, 0u64, 0u64, 0u64);
        let mut first_frame_ms = None;
        let mut last_detail = started;
        let mut next_status = started + Duration::from_secs(1);
        while started.elapsed() < Duration::from_secs(seconds) {
            if source.is_none() {
                match Source::open(&name, &mode) {
                    Ok(opened) => {
                        source = Some(opened);
                        opens += 1;
                    }
                    Err(error) => {
                        failures += 1;
                        println!(
                            "{}",
                            json!({"event":"open_error", "ms":started.elapsed().as_secs_f64()*1000.,"error":format!("{error:#}")})
                        );
                        timer.until(Instant::now() + Duration::from_millis(150));
                        continue;
                    }
                }
            }
            let detail = frames < 3 || last_detail.elapsed() >= Duration::from_secs(1);
            match source.as_mut().unwrap().next(detail) {
                Ok(Some(mut frame)) => {
                    frames += 1;
                    first_frame_ms.get_or_insert_with(|| started.elapsed().as_secs_f64() * 1000.);
                    if detail {
                        frame["event"] = json!("frame");
                        frame["ms"] = json!(started.elapsed().as_secs_f64() * 1000.);
                        frame["count"] = json!(frames);
                        println!("{frame}");
                        last_detail = Instant::now();
                    }
                }
                Ok(None) => {
                    empty += 1;
                    timer.until(Instant::now() + Duration::from_micros(500));
                }
                Err(error) => {
                    failures += 1;
                    println!(
                        "{}",
                        json!({"event":"capture_error", "ms":started.elapsed().as_secs_f64()*1000.,"error":format!("{error:#}")})
                    );
                    source = None;
                    timer.until(Instant::now() + Duration::from_millis(150));
                }
            }
            if Instant::now() >= next_status {
                println!(
                    "{}",
                    json!({"event":"status","ms":started.elapsed().as_secs_f64()*1000.,"frames":frames,"empty":empty,"failures":failures,"opens":opens})
                );
                next_status = Instant::now() + Duration::from_secs(1);
            }
        }
        println!(
            "{}",
            json!({"event":"done","ms":started.elapsed().as_secs_f64()*1000.,"frames":frames,"empty":empty,"failures":failures,"opens":opens,"first_frame_ms":first_frame_ms})
        );
        ensure!(frames > 0, "no frames received");
        Ok(())
    }
}
