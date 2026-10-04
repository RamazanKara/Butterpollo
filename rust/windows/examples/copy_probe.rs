//! How long a small copy takes on the GPU, for comparison with a busy GPU
//! (run beside gpu_load). Not packaged.
#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};
    use windows::Win32::Graphics::{
        Direct3D::D3D_DRIVER_TYPE_HARDWARE, Direct3D11::*, Dxgi::Common::*,
    };
    unsafe {
        // --class N: the process's GPU scheduling priority class, set before
        // the device exists.
        let args: Vec<String> = std::env::args().collect();
        if let Some(class) = args
            .iter()
            .position(|a| a == "--class")
            .and_then(|i| args.get(i + 1))
        {
            let library = libloading::Library::new("gdi32.dll")?;
            let set = library.get::<unsafe extern "system" fn(*mut std::ffi::c_void, i32) -> i32>(
                b"D3DKMTSetProcessSchedulingPriorityClass\0",
            )?;
            let status = set(
                windows::Win32::System::Threading::GetCurrentProcess().0,
                class.parse()?,
            );
            println!("scheduling class {class}: status {status:#x}");
        }
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
        if std::env::args().any(|a| a == "--priority") {
            let dxgi: windows::Win32::Graphics::Dxgi::IDXGIDevice =
                windows::core::Interface::cast(&device)?;
            dxgi.SetGPUThreadPriority(7)?;
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: 1968,
            Height: 2184,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let (mut a, mut b) = (None, None);
        device.CreateTexture2D(&desc, None, Some(&mut a))?;
        device.CreateTexture2D(&desc, None, Some(&mut b))?;
        let (a, b) = (a.unwrap(), b.unwrap());
        let mut query = None;
        device.CreateQuery(
            &D3D11_QUERY_DESC {
                Query: D3D11_QUERY_EVENT,
                MiscFlags: 0,
            },
            Some(&mut query),
        )?;
        let query = query.unwrap();
        let mut samples = vec![];
        for _ in 0..300 {
            let began = Instant::now();
            context.CopyResource(&b, &a);
            context.End(&query);
            context.Flush();
            loop {
                let mut done = windows::core::BOOL(0);
                let _ = context.GetData(
                    &query,
                    Some((&mut done as *mut windows::core::BOOL).cast()),
                    4,
                    0,
                );
                if done.as_bool() {
                    break;
                }
                std::thread::yield_now();
            }
            samples.push(began.elapsed().as_secs_f64() * 1000.);
            std::thread::sleep(Duration::from_millis(8));
        }
        samples.sort_by(f64::total_cmp);
        println!(
            "copy ms: mean {:.3} p50 {:.3} p95 {:.3}",
            samples.iter().sum::<f64>() / samples.len() as f64,
            samples[samples.len() / 2],
            samples[samples.len() * 95 / 100]
        );
    }
    Ok(())
}
