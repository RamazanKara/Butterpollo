//! Where capture time goes, per picture. A test-owned 8x1-pixel window in the
//! display's bottom-right corner shows a frame number; the probe captures it
//! with WGC in this process (`wgc`), WGC through the signed-in user's helper
//! process and its shared textures (`helper`), Desktop Duplication (`ddx`), or
//! WGC and DDX side by side (`both`, WGC direct, or helper under SYSTEM), and
//! times every new picture from the renderer's Present call to an owned
//! texture whose GPU copy has finished. Not packaged.
//!
//! usage: capture_phase_probe DISPLAY SECONDS RUNS OUT_PREFIX
//!   DISPLAY  `\\.\DISPLAYn`, empty for the primary, or `virtual:WxH@HZ` to
//!            create a virtual display for the probe's lifetime (SYSTEM only).
//!   RUNS     comma-separated `kind[@render_hz][+load]`, e.g.
//!            `wgc,helper@500,ddx+load`; @500 is vsync-paced: check the
//!            measured Present intervals rather than assuming a source rate.
//!            `+load` runs gpu_load.exe from this directory beside the run.
//! Build with `cargo build -p butterpollo-windows --example capture_phase_probe
//! --release --target-dir target/qa` in the Rust SDK environment. Put the
//! PyroWave runtime DLL beside the executable, and gpu_load.exe for `+load`.
//! `stamp_qpc` is the legacy capture timestamp, clamped on WGC; it is not
//! an independent DWM present measurement. Use wgc_stamp_probe for raw stamps.
//! Each run writes OUT_PREFIX-NN-kind[-load].json. No display mode, HDR or
//! layout changes beyond the optional virtual display. Checks for a connected
//! Moonlight client before each run and stops before starting another.
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
    let prefix = std::env::args().nth(4);
    let result = probe::run();
    if let (Err(error), Some(prefix)) = (&result, prefix) {
        let _ = std::fs::write(format!("{prefix}-error.txt"), format!("{error:#}"));
    }
    result
}

#[cfg(windows)]
mod probe {
    use anyhow::{Context, Result, bail, ensure};
    use butterpollo_core::config::Config;
    use butterpollo_windows::{
        capture::{Capture, ComGuard, Display, GpuImage, Priority, displays, enable_dpi_awareness},
        timing::{StreamingScope, Timer},
    };
    use std::{
        collections::BTreeMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicU64, Ordering},
            mpsc,
        },
        time::{Duration, Instant},
    };
    use windows::{
        Win32::{
            Foundation::*,
            Graphics::{
                Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
                Direct3D11::*,
                Dxgi::{Common::*, *},
            },
            System::{LibraryLoader::GetModuleHandleW, Performance::*, Threading::*},
            UI::WindowsAndMessaging::*,
        },
        core::{Interface, w},
    };

    const PIXELS: u32 = 8;

    fn qpc() -> i64 {
        let mut ticks = 0;
        unsafe {
            let _ = QueryPerformanceCounter(&mut ticks);
        }
        ticks
    }

    /// Rust's Instant is QPC on Windows; one reference pair converts exactly.
    struct Clock {
        instant: Instant,
        qpc: i64,
        frequency: f64,
    }
    impl Clock {
        fn new() -> Self {
            let mut frequency = 0;
            unsafe {
                let _ = QueryPerformanceFrequency(&mut frequency);
            }
            let instant = Instant::now();
            Self {
                instant,
                qpc: qpc(),
                frequency: frequency as f64,
            }
        }
        fn ticks(&self, at: Instant) -> i64 {
            let seconds = if at >= self.instant {
                at.duration_since(self.instant).as_secs_f64()
            } else {
                -self.instant.duration_since(at).as_secs_f64()
            };
            self.qpc + (seconds * self.frequency).round() as i64
        }
        fn ms(&self, ticks: i64) -> f64 {
            ticks as f64 * 1000. / self.frequency
        }
    }

    /// Seven pixels carry 21 bits of the frame number, three per pixel in
    /// R/G/B at 0 or 255; the eighth carries a checksum. Full-scale values
    /// survive colour management.
    fn encode(sequence: u32) -> [u8; PIXELS as usize * 4] {
        let mut bytes = [0u8; PIXELS as usize * 4];
        let check = (sequence.count_ones() + 3) & 7;
        for pixel in 0..PIXELS as usize {
            let bits = if pixel < 7 {
                (sequence >> (3 * pixel)) & 7
            } else {
                check
            };
            // B, G, R, A
            bytes[pixel * 4] = if bits & 4 != 0 { 255 } else { 0 };
            bytes[pixel * 4 + 1] = if bits & 2 != 0 { 255 } else { 0 };
            bytes[pixel * 4 + 2] = if bits & 1 != 0 { 255 } else { 0 };
            bytes[pixel * 4 + 3] = 255;
        }
        bytes
    }
    fn decode(bits: [u32; PIXELS as usize]) -> Option<u32> {
        let mut sequence = 0;
        for (pixel, value) in bits.iter().take(7).enumerate() {
            sequence |= value << (3 * pixel);
        }
        (sequence != 0 && (sequence.count_ones() + 3) & 7 == bits[7]).then_some(sequence)
    }

    #[derive(Clone, Copy)]
    struct Rendered {
        sequence: u32,
        before: i64,
        after: i64,
    }

    fn render(
        display: Display,
        hz: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
        rendered: Arc<Mutex<Vec<Rendered>>>,
        started: mpsc::Sender<std::result::Result<(), String>>,
    ) -> Result<()> {
        unsafe extern "system" fn procedure(
            hwnd: HWND,
            message: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        let setup = || -> Result<_> {
            unsafe {
                let module = GetModuleHandleW(None)?;
                let class = WNDCLASSW {
                    lpfnWndProc: Some(procedure),
                    hInstance: HINSTANCE(module.0),
                    lpszClassName: w!("ButterpolloCapturePhaseProbe"),
                    ..Default::default()
                };
                if RegisterClassW(&class) == 0 {
                    bail!("cannot register the test window class");
                }
                let x = display.x + display.width as i32 - PIXELS as i32;
                let y = display.y + display.height as i32 - 1;
                let window = CreateWindowExW(
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                    w!("ButterpolloCapturePhaseProbe"),
                    w!("Rubylight capture phase probe"),
                    WS_POPUP | WS_VISIBLE,
                    x,
                    y,
                    PIXELS as i32,
                    1,
                    None,
                    None,
                    Some(HINSTANCE(module.0)),
                    None,
                )?;
                SetWindowPos(
                    window,
                    Some(HWND_TOPMOST),
                    x,
                    y,
                    PIXELS as i32,
                    1,
                    SWP_NOACTIVATE,
                )?;
                let factory: IDXGIFactory2 = CreateDXGIFactory1()?;
                let adapter = factory.EnumAdapters1(display.adapter_index)?;
                let mut device = None;
                let mut context = None;
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )?;
                let device: ID3D11Device = device.context("no D3D11 device")?;
                let context = context.context("no D3D11 context")?;
                let swap: IDXGISwapChain2 = factory
                    .CreateSwapChainForHwnd(
                        &device,
                        window,
                        &DXGI_SWAP_CHAIN_DESC1 {
                            Width: PIXELS,
                            Height: 1,
                            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            SampleDesc: DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                            BufferCount: 2,
                            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                            Scaling: DXGI_SCALING_STRETCH,
                            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                            Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
                            ..Default::default()
                        },
                        None,
                        None,
                    )?
                    .cast()?;
                swap.SetMaximumFrameLatency(1)?;
                let wait = swap.GetFrameLatencyWaitableObject();
                ensure!(!wait.is_invalid(), "swap chain has no latency event");
                let mut source = None;
                device.CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Width: PIXELS,
                        Height: 1,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut source),
                )?;
                Ok((window, context, swap, wait, source.unwrap()))
            }
        };
        let (window, context, swap, wait, source) = match setup() {
            Ok(objects) => {
                let _ = started.send(Ok(()));
                objects
            }
            Err(error) => {
                let _ = started.send(Err(format!("{error:#}")));
                return Err(error);
            }
        };
        let timer = Timer::new()?;
        let mut next = Instant::now();
        let mut sequence = 0u32;
        unsafe {
            while !stop.load(Ordering::Acquire) {
                if WaitForSingleObject(wait, 1000) != WAIT_OBJECT_0 {
                    bail!("presentation event timed out");
                }
                let interval =
                    Duration::from_secs_f64(1. / f64::from_bits(hz.load(Ordering::Acquire)));
                timer.until(next);
                next = (next + interval).max(Instant::now());
                let mut message = MSG::default();
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                sequence += 1;
                let bytes = encode(sequence);
                context.UpdateSubresource(&source, 0, None, bytes.as_ptr().cast(), PIXELS * 4, 0);
                let back: ID3D11Texture2D = swap.GetBuffer(0)?;
                context.CopyResource(&back, &source);
                let before = qpc();
                swap.Present(1, DXGI_PRESENT(0)).ok()?;
                let after = qpc();
                rendered.lock().unwrap().push(Rendered {
                    sequence,
                    before,
                    after,
                });
            }
            let _ = CloseHandle(wait);
            let _ = DestroyWindow(window);
        }
        Ok(())
    }

    #[derive(Default)]
    struct Reader {
        staging: Option<ID3D11Texture2D>,
        query: Option<ID3D11Query>,
        format: i32,
    }
    struct Seen {
        sequence: Option<u32>,
        captured: i64,
        acquired: i64,
        ready: i64,
    }
    impl Reader {
        /// Wait for the owned copy on the GPU, then read the test pixels.
        fn sample(&mut self, image: &GpuImage, clock: &Clock) -> Result<Seen> {
            unsafe {
                let gpu = &image.gpu;
                if self.query.is_none() {
                    let mut query = None;
                    gpu.device.CreateQuery(
                        &D3D11_QUERY_DESC {
                            Query: D3D11_QUERY_EVENT,
                            MiscFlags: 0,
                        },
                        Some(&mut query),
                    )?;
                    self.query = query;
                }
                let query = self.query.as_ref().unwrap();
                gpu.context.End(query);
                gpu.context.Flush();
                loop {
                    let mut done = 0i32;
                    gpu.context.GetData(
                        query,
                        Some((&mut done as *mut i32).cast()),
                        size_of::<i32>() as u32,
                        0,
                    )?;
                    if done != 0 {
                        break;
                    }
                    std::hint::spin_loop();
                }
                let ready = qpc();
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                image.texture.GetDesc(&mut desc);
                self.format = desc.Format.0;
                let stale = self.staging.as_ref().is_none_or(|staging| {
                    let mut old = D3D11_TEXTURE2D_DESC::default();
                    staging.GetDesc(&mut old);
                    old.Format != desc.Format
                });
                if stale {
                    let mut staging = None;
                    gpu.device.CreateTexture2D(
                        &D3D11_TEXTURE2D_DESC {
                            Width: PIXELS,
                            Height: 1,
                            MipLevels: 1,
                            ArraySize: 1,
                            Format: desc.Format,
                            SampleDesc: DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            Usage: D3D11_USAGE_STAGING,
                            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                            ..Default::default()
                        },
                        None,
                        Some(&mut staging),
                    )?;
                    self.staging = staging;
                }
                let staging = self.staging.as_ref().unwrap();
                gpu.context.CopySubresourceRegion(
                    staging,
                    0,
                    0,
                    0,
                    0,
                    image.texture.as_ref(),
                    0,
                    Some(&D3D11_BOX {
                        left: desc.Width - PIXELS,
                        top: desc.Height - 1,
                        front: 0,
                        right: desc.Width,
                        bottom: desc.Height,
                        back: 1,
                    }),
                );
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                gpu.context
                    .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                let mut bits = [0u32; PIXELS as usize];
                for (pixel, value) in bits.iter_mut().enumerate() {
                    *value = match desc.Format {
                        DXGI_FORMAT_B8G8R8A8_UNORM => {
                            let p = std::slice::from_raw_parts(
                                (mapped.pData as *const u8).add(pixel * 4),
                                4,
                            );
                            u32::from(p[2] > 127)
                                | u32::from(p[1] > 127) << 1
                                | u32::from(p[0] > 127) << 2
                        }
                        DXGI_FORMAT_R16G16B16A16_FLOAT => {
                            let p = std::slice::from_raw_parts(
                                (mapped.pData as *const u16).add(pixel * 4),
                                4,
                            );
                            // Positive and at least 0.5 (half 0x3800).
                            let on = |h: u16| h & 0x8000 == 0 && h >= 0x3800;
                            u32::from(on(p[0]))
                                | u32::from(on(p[1])) << 1
                                | u32::from(on(p[2])) << 2
                        }
                        other => bail!("unsupported capture format {other:?}"),
                    };
                }
                gpu.context.Unmap(staging, 0);
                Ok(Seen {
                    sequence: decode(bits),
                    captured: clock.ticks(image.captured),
                    acquired: clock.ticks(image.acquired),
                    ready,
                })
            }
        }
    }

    fn stats(mut values: Vec<f64>) -> serde_json::Value {
        if values.is_empty() {
            return serde_json::Value::Null;
        }
        values.sort_by(f64::total_cmp);
        let at = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];
        serde_json::json!({
            "n": values.len(),
            "mean": values.iter().sum::<f64>() / values.len() as f64,
            "p05": at(0.05), "p50": at(0.5), "p95": at(0.95), "p99": at(0.99),
            "max": values[values.len() - 1],
        })
    }

    fn stream_active() -> bool {
        let Ok(text) = std::fs::read(r"C:\ProgramData\Butterpollo\config\logs\butterpollo.log")
        else {
            return false;
        };
        let tail = &text[text.len().saturating_sub(2_000_000)..];
        let tail = String::from_utf8_lossy(tail);
        let connected = tail.rfind("CLIENT CONNECTED");
        let disconnected = tail.rfind("CLIENT DISCONNECTED");
        match (connected, disconnected) {
            (Some(c), Some(d)) => c > d,
            (Some(_), None) => true,
            _ => false,
        }
    }

    struct Load(std::process::Child);
    impl Drop for Load {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    pub fn run() -> Result<()> {
        let mut args = std::env::args().skip(1);
        let usage = "usage: capture_phase_probe DISPLAY SECONDS RUNS OUT_PREFIX";
        let name = args.next().context(usage)?;
        let seconds: f64 = args.next().context(usage)?.parse()?;
        let runs = args.next().context(usage)?;
        let prefix = args.next().context(usage)?;
        ensure!((3. ..=60.).contains(&seconds), "seconds must be 3..60");
        ensure!(
            !stream_active(),
            "a Moonlight client is connected to the installed host"
        );
        enable_dpi_awareness();
        let _com = ComGuard::new()?;
        let system = butterpollo_windows::process::is_system();
        // Create the optional virtual display and keep its lease fed.
        let stop = Arc::new(AtomicBool::new(false));
        let mut feeder = None;
        let name = if let Some(spec) = name.strip_prefix("virtual:") {
            let (size, rate) = spec.split_once('@').context("virtual:WxH@HZ")?;
            let (width, height) = size.split_once('x').context("virtual:WxH@HZ")?;
            let virtual_display = butterpollo_windows::display::VirtualDisplay::create(
                "capture-phase-probe",
                width.parse()?,
                height.parse()?,
                rate.parse()?,
            )?;
            let output = virtual_display.name.clone();
            let lease = Arc::new(Mutex::new(virtual_display));
            let stop = stop.clone();
            feeder = Some(std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    if let Err(error) = lease.lock().unwrap().feed() {
                        eprintln!("virtual display feed failed: {error:#}");
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                drop(lease);
            }));
            std::thread::sleep(Duration::from_secs(3));
            output
        } else {
            name
        };
        let result = (|| -> Result<()> {
            // `hold:SECONDS` only keeps the display for a probe in another session.
            if let Some(hold) = runs.strip_prefix("hold:") {
                std::fs::write(format!("{prefix}-display.txt"), &name)?;
                let until = Instant::now() + Duration::from_secs(hold.parse::<u64>()?.min(600));
                while Instant::now() < until && !stream_active() {
                    std::thread::sleep(Duration::from_millis(500));
                }
                let _ = std::fs::remove_file(format!("{prefix}-display.txt"));
                return Ok(());
            }
            let display = displays()?
                .into_iter()
                .find(|d| {
                    name.is_empty() && d.primary || d.display_name.eq_ignore_ascii_case(&name)
                })
                .context("display is not active")?;
            let mode = butterpollo_windows::display::mode(&display.display_name)?;
            let clock = Clock::new();
            let hz = Arc::new(AtomicU64::new(60f64.to_bits()));
            let rendered = Arc::new(Mutex::new(Vec::<Rendered>::new()));
            let (started_tx, started_rx) = mpsc::channel();
            let renderer = {
                let display = display.clone();
                let stop = stop.clone();
                let hz = hz.clone();
                let rendered = rendered.clone();
                std::thread::spawn(move || render(display, hz, stop, rendered, started_tx))
            };
            started_rx
                .recv_timeout(Duration::from_secs(10))?
                .map_err(|e| anyhow::anyhow!(e))?;
            let _scope = StreamingScope::enter();
            let _priority = Priority::new();
            let timer = Timer::new()?;
            for (index, run) in runs.split(',').enumerate() {
                ensure!(!stream_active(), "a Moonlight client connected; stopping");
                let (run, load) = match run.strip_suffix("+load") {
                    Some(run) => (run, true),
                    None => (run, false),
                };
                let (kind, rate) = match run.split_once('@') {
                    Some((kind, rate)) => (kind, rate.parse::<f64>()?),
                    None => (run, 59.3),
                };
                ensure!(
                    (1. ..=1000.).contains(&rate),
                    "render rate must be 1..1000 Hz"
                );
                hz.store(rate.to_bits(), Ordering::Release);
                // Direct WGC cannot open under SYSTEM; `both` pairs DDX with the helper there.
                let direct = if system {
                    "wgc_user_helper = true\n"
                } else {
                    "wgc_user_helper = false\n"
                };
                let kinds: Vec<(&str, &str, &str)> = match kind {
                    "wgc" => vec![("wgc", "wgc", "wgc_user_helper = false\n")],
                    "helper" => vec![("helper", "wgc", "wgc_user_helper = true\n")],
                    "ddx" => vec![("ddx", "ddx", "")],
                    "both" => vec![
                        (if system { "helper" } else { "wgc" }, "wgc", direct),
                        ("ddx", "ddx", ""),
                    ],
                    _ => bail!("unknown run kind {kind}"),
                };
                let _load = if load {
                    let exe = std::env::current_exe()?.with_file_name("gpu_load.exe");
                    let child = std::process::Command::new(exe)
                        .args([
                            (seconds + 5.).ceil().to_string().as_str(),
                            "1000",
                            "0",
                            "200",
                        ])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()?;
                    std::thread::sleep(Duration::from_millis(1500));
                    Some(Load(child))
                } else {
                    None
                };
                let mut captures = Vec::new();
                for (label, backend, config) in &kinds {
                    let config = Config::parse(config)?;
                    let capture =
                        Capture::new_options(&display.display_name, backend, false, &config)
                            .with_context(|| format!("open {label}"))?;
                    ensure!(
                        capture.backend() == *backend,
                        "{label} opened {}",
                        capture.backend()
                    );
                    captures.push((*label, capture, Reader::default(), Vec::<Seen>::new()));
                }
                let start = Instant::now();
                let end = start + Duration::from_secs_f64(seconds);
                let mut empty = 0u64;
                while Instant::now() < end {
                    let mut any = false;
                    for (_, capture, reader, seen) in captures.iter_mut() {
                        if let Some(signal) = capture.frame_signal() {
                            signal.reset()?;
                        }
                        if let Some(image) = capture.next_gpu()? {
                            any = true;
                            seen.push(reader.sample(&image, &clock)?);
                        }
                    }
                    if !any {
                        empty += 1;
                        let until = Instant::now() + Duration::from_micros(500);
                        match captures.first().and_then(|(_, c, _, _)| c.frame_signal()) {
                            Some(signal) if captures.len() == 1 => {
                                timer.until_or_signal(until, signal)?;
                            }
                            _ => timer.until(until),
                        }
                    }
                }
                let finished = Instant::now();
                let window = (
                    clock.ticks(start + Duration::from_secs(1)),
                    clock.ticks(finished - Duration::from_millis(250)),
                );
                let all: Vec<Rendered> = rendered.lock().unwrap().clone();
                let presents: BTreeMap<u32, Rendered> =
                    all.iter().map(|r| (r.sequence, *r)).collect();
                let expected: Vec<&Rendered> = all
                    .iter()
                    .filter(|r| r.before >= window.0 && r.before <= window.1)
                    .collect();
                let intervals: Vec<f64> = expected
                    .windows(2)
                    .map(|pair| clock.ms(pair[1].before - pair[0].before))
                    .collect();
                let mut report = serde_json::json!({
                    "scope": "renderer Present call to an owned capture texture whose GPU copy is done; excludes claim, conversion and encoding",
                    "display": display.display_name, "width": display.width, "height": display.height,
                    "refresh_hz": mode.dmDisplayFrequency, "system": system,
                    "run": run, "load": load, "render_hz": rate,
                    "seconds": finished.duration_since(start).as_secs_f64(),
                    "rendered_in_window": expected.len(),
                    "present_interval_ms": stats(intervals),
                    "present_call_ms": stats(expected.iter().map(|r| clock.ms(r.after - r.before)).collect()),
                    "empty_polls": empty,
                });
                let mut raw = serde_json::Map::new();
                let mut firsts = Vec::new();
                for (label, _, reader, seen) in &captures {
                    let mut first: BTreeMap<u32, &Seen> = BTreeMap::new();
                    let mut undecoded = 0;
                    for s in seen {
                        match s.sequence {
                            Some(sequence) => {
                                first.entry(sequence).or_insert(s);
                            }
                            None => undecoded += 1,
                        }
                    }
                    let rows: Vec<(Rendered, &Seen)> = first
                        .iter()
                        .filter_map(|(sequence, s)| {
                            presents
                                .get(sequence)
                                .filter(|r| r.before >= window.0 && r.before <= window.1)
                                .map(|r| (*r, *s))
                        })
                        .collect();
                    let column = |f: &dyn Fn(&Rendered, &Seen) -> i64| -> Vec<f64> {
                        rows.iter().map(|(r, s)| clock.ms(f(r, s))).collect()
                    };
                    report[*label] = serde_json::json!({
                        "pictures": seen.len(), "undecoded": undecoded, "format": reader.format,
                        "fresh_in_window": rows.len(),
                        "coverage": rows.len() as f64 / expected.len().max(1) as f64,
                        "present_to_stamp_ms": stats(column(&|r, s| s.captured - r.before)),
                        "stamp_to_acquired_ms": stats(column(&|_, s| s.acquired - s.captured)),
                        "acquired_to_copied_ms": stats(column(&|_, s| s.ready - s.acquired)),
                        "present_to_acquired_ms": stats(column(&|r, s| s.acquired - r.before)),
                        "present_to_copied_ms": stats(column(&|r, s| s.ready - r.before)),
                    });
                    raw.insert(
                        label.to_string(),
                        rows.iter()
                            .map(|(r, s)| {
                                serde_json::json!([
                                    r.sequence, r.before, s.captured, s.acquired, s.ready
                                ])
                            })
                            .collect(),
                    );
                    let map: BTreeMap<u32, (i64, i64, i64)> = rows
                        .iter()
                        .map(|(r, s)| (r.sequence, (s.captured, s.acquired, s.ready)))
                        .collect();
                    firsts.push(map);
                }
                if firsts.len() == 2 {
                    let mut stamp = Vec::new();
                    let mut acquired = Vec::new();
                    let mut copied = Vec::new();
                    for (sequence, x) in &firsts[0] {
                        if let Some(y) = firsts[1].get(sequence) {
                            stamp.push(clock.ms(x.0 - y.0));
                            acquired.push(clock.ms(x.1 - y.1));
                            copied.push(clock.ms(x.2 - y.2));
                        }
                    }
                    report["first_minus_second"] = serde_json::json!({
                        "stamp_ms": stats(stamp), "acquired_ms": stats(acquired), "copied_ms": stats(copied),
                    });
                }
                report["raw"] = serde_json::Value::Object(raw);
                report["raw_columns"] = serde_json::json!([
                    "sequence",
                    "present_qpc",
                    "stamp_qpc",
                    "acquired_qpc",
                    "copied_qpc"
                ]);
                report["qpc_frequency"] = serde_json::json!(clock.frequency);
                std::fs::write(
                    format!(
                        "{prefix}-{:02}-{kind}{}.json",
                        index + 1,
                        if load { "-load" } else { "" }
                    ),
                    serde_json::to_vec(&report)?,
                )?;
                drop(captures);
                std::thread::sleep(Duration::from_millis(500));
            }
            stop.store(true, Ordering::Release);
            renderer.join().unwrap()?;
            Ok(())
        })();
        stop.store(true, Ordering::Release);
        if let Some(feeder) = feeder {
            let _ = feeder.join();
        }
        result
    }
}
