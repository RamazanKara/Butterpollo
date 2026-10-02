//! Test-owned moving desktop with a timestamp in the pixels. Not packaged.
//! Pair with tests/moonlight_client.c to measure picture age independently of
//! either host's processing counter. Requires an explicitly selected display.
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
    use anyhow::{Context, Result, bail};
    use butterpollo_windows::capture::{ComGuard, displays, enable_dpi_awareness};
    use std::time::{Duration, Instant};
    use windows::{
        Win32::{
            Foundation::*,
            Graphics::{
                Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, Fxc::*},
                Direct3D11::*,
                Dxgi::{Common::*, *},
            },
            System::{LibraryLoader::GetModuleHandleW, Performance::*, Threading::*},
            UI::WindowsAndMessaging::*,
        },
        core::{Interface, PCSTR, w},
    };

    const SHADER: &str = r#"
cbuffer Frame : register(b0) { uint frame; uint ticksLow; uint ticksHigh; uint unused; };
float4 vertex(uint id : SV_VertexID) : SV_Position {
    float2 p = float2((id << 1) & 2, id & 2);
    return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
}
float4 picture(float4 p : SV_Position) : SV_Target {
    // Four rows: sequence, QPC low, QPC high, and a signature. Calibration
    // patches at x=12/36 let the decoder read SDR and PQ HDR with the same test.
    if (p.y >= 8 && p.y < 104) {
        if (p.x < 24) return float4(0, 0, 0, 1);
        if (p.x < 48) return float4(1.25, 1.25, 1.25, 1);
        if (p.x >= 64 && p.x < 576) {
            uint row = uint(p.y - 8) / 24;
            uint word = row == 0 ? frame : row == 1 ? ticksLow : row == 2 ? ticksHigh : 0xB17E2212;
            float value = ((word >> (uint(p.x - 64) / 16)) & 1) * 1.25;
            return float4(value, value, value, 1);
        }
    }
    // Deterministic motion with edges, gradients and HDR highlights. No
    // expensive scene rendering that would obscure the host's own costs.
    float2 at = p.xy + float2(frame * 4, frame * 2);
    float grid = fmod(floor(at.x / 32) + floor(at.y / 32), 2);
    float3 color = lerp(float3(0.02, 0.06, 0.12), float3(0.4, 0.22, 0.08), grid);
    float bar = fmod(at.x, 512);
    color += bar / 1024;
    if (bar < 8) color = float3(4, 2, 1);
    return float4(color, 1);
}
"#;

    struct Window(HWND);
    impl Drop for Window {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }
    struct Wait(HANDLE);
    impl Drop for Wait {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    unsafe extern "system" fn window(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }
    unsafe fn compile(entry: &'static [u8], target: &'static [u8]) -> Result<Vec<u8>> {
        unsafe {
            let mut code = None;
            let mut errors = None;
            let result = D3DCompile(
                SHADER.as_ptr().cast(),
                SHADER.len(),
                PCSTR::null(),
                None,
                None,
                PCSTR(entry.as_ptr()),
                PCSTR(target.as_ptr()),
                D3DCOMPILE_OPTIMIZATION_LEVEL3,
                0,
                &mut code,
                Some(&mut errors),
            );
            if let Err(error) = result {
                let details = errors
                    .map(|blob| {
                        String::from_utf8_lossy(std::slice::from_raw_parts(
                            blob.GetBufferPointer().cast(),
                            blob.GetBufferSize(),
                        ))
                        .into_owned()
                    })
                    .unwrap_or_default();
                bail!("motion shader: {error}: {details}");
            }
            let code = code.context("shader compiler returned no bytecode")?;
            Ok(
                std::slice::from_raw_parts(code.GetBufferPointer().cast(), code.GetBufferSize())
                    .to_vec(),
            )
        }
    }

    pub fn run() -> Result<()> {
        let mut args = std::env::args().skip(1);
        let display_name = args
            .next()
            .context("usage: motion_probe DISPLAY SECONDS [REPORT.json [SOURCE_HZ]]")?;
        let seconds: u64 = args.next().context("duration is required")?.parse()?;
        let report = args.next();
        let source_hz: Option<u32> = args.next().map(|s| s.parse()).transpose()?;
        if !(1..=300).contains(&seconds) || args.next().is_some() {
            bail!("duration must be 1..300 seconds");
        }
        if source_hz.is_some_and(|hz| !(1..=1000).contains(&hz)) {
            bail!("source refresh must be 1..1000 Hz");
        }
        enable_dpi_awareness();
        let _com = ComGuard::new()?;
        let display = displays()?
            .into_iter()
            .find(|d| d.display_name.eq_ignore_ascii_case(&display_name))
            .context("explicitly selected display is not active")?;
        if let Some(hz) = source_hz {
            butterpollo_windows::display::set_mode(
                &display.display_name,
                display.width,
                display.height,
                hz,
            )?;
        }
        println!("MOTION stage=display name={}", display.display_name);
        if display.width < 640 || display.height < 128 {
            bail!("the motion barcode needs a display of at least 640 by 128");
        }
        unsafe {
            let module = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(window),
                hInstance: HINSTANCE(module.0),
                lpszClassName: w!("ButterpolloMotionProbe"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                bail!("cannot register test window");
            }
            let window = Window(CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("ButterpolloMotionProbe"),
                w!("Butterpollo test motion"),
                WS_POPUP | WS_VISIBLE,
                display.x,
                display.y,
                display.width as i32,
                display.height as i32,
                None,
                None,
                Some(HINSTANCE(module.0)),
                None,
            )?);
            SetWindowPos(
                window.0,
                Some(HWND_TOPMOST),
                display.x,
                display.y,
                display.width as i32,
                display.height as i32,
                SWP_NOACTIVATE,
            )?;
            println!("MOTION stage=window");
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
            let device = device.context("no D3D11 device")?;
            let context = context.context("no D3D11 context")?;
            println!("MOTION stage=device");
            let swap = factory.CreateSwapChainForHwnd(
                &device,
                window.0,
                &DXGI_SWAP_CHAIN_DESC1 {
                    Width: display.width,
                    Height: display.height,
                    Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
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
            )?;
            let swap: IDXGISwapChain3 = swap.cast()?;
            println!("MOTION stage=swap-chain");
            swap.SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709)?;
            swap.SetMaximumFrameLatency(1)?;
            let wait = Wait(swap.GetFrameLatencyWaitableObject());
            if wait.0.is_invalid() {
                bail!("swap chain has no presentation event");
            }
            let texture: ID3D11Texture2D = swap.GetBuffer(0)?;
            let mut target = None;
            device.CreateRenderTargetView(&texture, None, Some(&mut target))?;
            let mut vertex = None;
            let mut picture = None;
            device.CreateVertexShader(
                &compile(b"vertex\0", b"vs_5_0\0")?,
                None,
                Some(&mut vertex),
            )?;
            device.CreatePixelShader(
                &compile(b"picture\0", b"ps_5_0\0")?,
                None,
                Some(&mut picture),
            )?;
            let mut constants = None;
            device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 16,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut constants),
            )?;
            let constants = constants.unwrap();
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(vertex.as_ref(), None);
            context.PSSetShader(picture.as_ref(), None);
            context.PSSetConstantBuffers(0, Some(&[Some(constants.clone())]));
            context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                Width: display.width as f32,
                Height: display.height as f32,
                MinDepth: 0.,
                MaxDepth: 1.,
                ..Default::default()
            }]));
            let mut frequency = 0;
            QueryPerformanceFrequency(&mut frequency)?;
            println!("MOTION stage=render-ready");
            let start = Instant::now();
            let mut frames = Vec::new();
            while start.elapsed() < Duration::from_secs(seconds) {
                if WaitForSingleObject(wait.0, 1000) != WAIT_OBJECT_0 {
                    bail!("presentation event timed out");
                }
                let mut message = MSG::default();
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    if message.message == WM_QUIT {
                        bail!("test window closed");
                    }
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                let mut ticks = 0;
                QueryPerformanceCounter(&mut ticks)?;
                let frame = frames.len() as u32 + 1;
                let values = [frame, ticks as u32, (ticks as u64 >> 32) as u32, 0];
                context.UpdateSubresource(&constants, 0, None, values.as_ptr().cast(), 0, 0);
                context.OMSetRenderTargets(Some(&[target.clone()]), None);
                context.Draw(3, 0);
                swap.Present(1, DXGI_PRESENT(0)).ok()?;
                if frame == 1 {
                    println!("MOTION stage=first-present");
                }
                frames.push(serde_json::json!({"frame":frame,"qpc":ticks}));
            }
            let data = serde_json::json!({
                "scope":"render timestamp to independent decode; excludes remote display scanout",
                "display":display,"requested_source_hz":source_hz,"seconds":start.elapsed().as_secs_f64(),
                "qpc_frequency":frequency,"presented_frames":frames.len(),"frames":frames
            });
            if let Some(path) = report {
                std::fs::write(path, serde_json::to_vec_pretty(&data)?)?;
            }
            println!(
                "MOTION frames={} seconds={:.3} display={}",
                frames.len(),
                start.elapsed().as_secs_f64(),
                display.display_name
            );
        }
        Ok(())
    }
}
