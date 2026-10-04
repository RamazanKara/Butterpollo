//! GPU load like a game's: a heavy full-screen pixel shader drawn to an
//! offscreen target, uncapped or at a frame rate. For measuring how the
//! host's conversion and encoding fare beside a busy GPU. Not packaged.
//!
//! usage: gpu_load SECONDS ITERATIONS [FPS]
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows.");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::{Context, bail};
    use std::time::{Duration, Instant};
    use windows::{
        Win32::Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, Fxc::*},
            Direct3D11::*,
            Dxgi::Common::*,
        },
        core::PCSTR,
    };
    const SHADER: &str = r#"
cbuffer Frame : register(b0) { uint frame; uint iterations; uint pad0; uint pad1; };
float4 vertex(uint id : SV_VertexID) : SV_Position {
    float2 p = float2((id << 1) & 2, id & 2);
    return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
}
float4 picture(float4 p : SV_Position) : SV_Target {
    float3 c = float3(p.xy / 2048.0, frame * 0.001);
    [loop] for (uint i = 0; i < iterations; i++) {
        c = frac(sin(c * 12.9898 + c.yzx * 78.233) * 43758.5453);
    }
    return float4(c, 1);
}
"#;
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args
        .next()
        .context("usage: gpu_load SECONDS ITERATIONS [FPS]")?
        .parse()?;
    let iterations: u32 = args.next().context("iterations required")?.parse()?;
    let fps: Option<f64> = args
        .next()
        .filter(|s| s != "0")
        .map(|s| s.parse())
        .transpose()?;
    // A game's frame is many draws; each takes a share of the work.
    let draws: u32 = args.next().map_or(Ok(1), |s| s.parse())?.max(1);
    let compile = |entry: &[u8], target: &[u8]| -> anyhow::Result<Vec<u8>> {
        unsafe {
            let mut code = None;
            let mut errors = None;
            if let Err(error) = D3DCompile(
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
            ) {
                bail!("shader: {error}");
            }
            let code = code.context("no bytecode")?;
            Ok(std::slice::from_raw_parts(
                code.GetBufferPointer().cast::<u8>(),
                code.GetBufferSize(),
            )
            .to_vec())
        }
    };
    unsafe {
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        let (device, context): (ID3D11Device, ID3D11DeviceContext) =
            (device.unwrap(), context.unwrap());
        let (width, height) = (2560u32, 1440u32);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let mut texture = None;
        device.CreateTexture2D(&desc, None, Some(&mut texture))?;
        let mut view = None;
        device.CreateRenderTargetView(&texture.unwrap(), None, Some(&mut view))?;
        let vs = compile(b"vertex\0", b"vs_5_0\0")?;
        let ps = compile(b"picture\0", b"ps_5_0\0")?;
        let (mut vertex, mut pixel) = (None, None);
        device.CreateVertexShader(&vs, None, Some(&mut vertex))?;
        device.CreatePixelShader(&ps, None, Some(&mut pixel))?;
        let buffer_desc = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let mut constants = None;
        device.CreateBuffer(&buffer_desc, None, Some(&mut constants))?;
        let constants = constants.unwrap();
        let query_desc = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        };
        let mut done = None;
        device.CreateQuery(&query_desc, Some(&mut done))?;
        let done = done.unwrap();
        context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        context.VSSetShader(vertex.as_ref(), None);
        context.PSSetShader(pixel.as_ref(), None);
        context.PSSetConstantBuffers(0, Some(&[Some(constants.clone())]));
        context.OMSetRenderTargets(Some(&[view.clone()]), None);
        context.RSSetViewports(Some(&[D3D11_VIEWPORT {
            Width: width as f32,
            Height: height as f32,
            MaxDepth: 1.,
            ..Default::default()
        }]));
        let start = Instant::now();
        let end = start + Duration::from_secs(seconds);
        let mut frame = 0u32;
        let mut gpu_ms = vec![];
        let mut due = start;
        while Instant::now() < end {
            if let Some(fps) = fps {
                while Instant::now() < due {
                    std::thread::sleep(Duration::from_micros(200));
                }
                due += Duration::from_secs_f64(1. / fps);
            }
            let values = [frame, (iterations / draws).max(1), 0, 0];
            context.UpdateSubresource(&constants, 0, None, values.as_ptr().cast(), 0, 0);
            let began = Instant::now();
            for _ in 0..draws {
                context.Draw(3, 0);
            }
            context.End(&done);
            context.Flush();
            // A game keeps at most a frame or two queued.
            loop {
                let mut finished = windows::core::BOOL(0);
                let _ = context.GetData(
                    &done,
                    Some((&mut finished as *mut windows::core::BOOL).cast()),
                    4,
                    0,
                );
                if finished.as_bool() {
                    break;
                }
                std::thread::yield_now();
            }
            gpu_ms.push(began.elapsed().as_secs_f64() * 1000.);
            frame += 1;
        }
        gpu_ms.sort_by(f64::total_cmp);
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "{{\"frames\":{frame},\"fps\":{:.1},\"frame_ms_p50\":{:.3},\"frame_ms_p95\":{:.3}}}",
            f64::from(frame) / elapsed,
            gpu_ms[gpu_ms.len() / 2],
            gpu_ms[gpu_ms.len() * 95 / 100]
        );
    }
    Ok(())
}
