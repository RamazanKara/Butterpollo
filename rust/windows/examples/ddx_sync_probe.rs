//! How long a Desktop Duplication frame takes to become readable from a
//! D3D12 compute queue: the D3D11 device signals a shared fence after
//! acquiring the frame (its keyed-mutex wait for DWM's copy comes first),
//! and the time until that fence completes is reported, with the compute
//! copy that follows. Run beside gpu_load to see the graphics queue's share.
//! Not packaged. usage: ddx_sync_probe DISPLAY SECONDS [plain|verify|verify-nosync|nosync]
//! Frames are copied as the capture does (`compute::Handoff`), or with
//! `nosync` by a plain compute-queue copy that ignores DWM. `verify` then
//! copies the frame on the D3D11 device too (which waits for DWM through the
//! keyed mutex) and counts the frames where the two differ.
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows.");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::Context;
    use std::time::{Duration, Instant};
    use windows::{
        Win32::Graphics::{Direct3D11::*, Direct3D12::*, Dxgi::Common::*, Dxgi::*},
        core::Interface,
    };
    butterpollo_windows::capture::enable_dpi_awareness();
    let mut args = std::env::args().skip(1);
    let name = args.next().unwrap_or_default();
    let seconds: u64 = args.next().map_or(Ok(10), |s| s.parse())?;
    let mode = args.next().unwrap_or_default();
    let sync = !mode.contains("nosync");
    let verify = mode.contains("verify");
    let gpu = butterpollo_windows::capture::Device::new(&name)?;
    let compute = butterpollo_windows::compute::Compute::for_device(&gpu.device)?;
    let stats = |label: &str, values: &mut Vec<f64>| {
        if values.is_empty() {
            println!("{label}: no samples");
            return;
        }
        values.sort_by(f64::total_cmp);
        let at = |q: f64| values[((values.len() - 1) as f64 * q) as usize];
        println!(
            "{label}: n {} mean {:.3} p50 {:.3} p95 {:.3} p99 {:.3} max {:.3} ms",
            values.len(),
            values.iter().sum::<f64>() / values.len() as f64,
            at(0.5),
            at(0.95),
            at(0.99),
            values[values.len() - 1]
        );
    };
    unsafe {
        let fence12: ID3D12Fence = compute.device.CreateFence(0, D3D12_FENCE_FLAG_SHARED)?;
        let handle = compute
            .device
            .CreateSharedHandle(&fence12, None, 0x10000000, None)?;
        let device5: ID3D11Device5 = gpu.device.cast()?;
        let mut fence11: Option<ID3D11Fence> = None;
        device5.OpenSharedFence(handle, &mut fence11)?;
        let fence11 = fence11.context("no D3D11 fence")?;
        let context4: ID3D11DeviceContext4 = gpu.context.cast()?;
        let output = gpu.output()?.cast::<IDXGIOutput5>()?;
        let duplicate = output.DuplicateOutput1(
            &gpu.device,
            0,
            &[DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_B8G8R8A8_UNORM],
        )?;
        let mut copy: Option<(ID3D11Texture2D, ID3D12Resource)> = None;
        let mut handoff = butterpollo_windows::compute::Handoff::new(compute.clone(), &gpu)?;
        let raw = {
            let allocator: ID3D12CommandAllocator = compute
                .device
                .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COMPUTE)?;
            let list: ID3D12GraphicsCommandList = compute.device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_COMPUTE,
                &allocator,
                None,
            )?;
            list.Close()?;
            (allocator, list)
        };
        let mut signal = Vec::new();
        let mut copies = Vec::new();
        let mut total = Vec::new();
        let mut plain = Vec::new();
        let mut value = 0u64;
        let (mut checked, mut differing) = (0u32, 0u32);
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < deadline {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource = None;
            match duplicate.AcquireNextFrame(100, &mut info, &mut resource) {
                Ok(()) => {}
                Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
                Err(error) => return Err(error.into()),
            }
            if info.LastPresentTime != 0 {
                let texture: ID3D11Texture2D = resource.context("no frame")?.cast()?;
                let started = Instant::now();
                if mode.is_empty() {
                    value += 1;
                    context4.Signal(&fence11, value)?;
                    gpu.context.Flush();
                    while fence12.GetCompletedValue() < value {
                        std::hint::spin_loop();
                    }
                    signal.push(started.elapsed().as_secs_f64() * 1000.);
                }
                if copy.is_none() {
                    let mut desc = D3D11_TEXTURE2D_DESC::default();
                    texture.GetDesc(&mut desc);
                    desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
                    desc.MiscFlags = (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0
                        | D3D11_RESOURCE_MISC_SHARED.0) as u32;
                    let mut target = None;
                    gpu.device.CreateTexture2D(&desc, None, Some(&mut target))?;
                    let target = target.unwrap();
                    let opened = compute.open(&target)?;
                    copy = Some((target, opened));
                }
                let copied = Instant::now();
                if sync {
                    // As the capture copies: ordered on the GPU, then waited for here.
                    handoff.copy(&copy.as_ref().unwrap().0, &texture)?.wait()?;
                } else {
                    // A plain copy on the compute queue, unordered with DWM.
                    raw.0.Reset()?;
                    raw.1.Reset(&raw.0, None)?;
                    raw.1
                        .CopyResource(&copy.as_ref().unwrap().1, &compute.open(&texture)?);
                    raw.1.Close()?;
                    compute
                        .queue
                        .ExecuteCommandLists(&[Some(raw.1.cast::<ID3D12CommandList>()?)]);
                    value += 1;
                    compute.queue.Signal(&fence12, value)?;
                    fence12.SetEventOnCompletion(value, Default::default())?;
                }
                copies.push(copied.elapsed().as_secs_f64() * 1000.);
                total.push(started.elapsed().as_secs_f64() * 1000.);
                if verify {
                    let compare = |source: &ID3D11Texture2D| -> anyhow::Result<Vec<u8>> {
                        let mut desc = D3D11_TEXTURE2D_DESC::default();
                        source.GetDesc(&mut desc);
                        desc.Usage = D3D11_USAGE_STAGING;
                        desc.BindFlags = 0;
                        desc.MiscFlags = 0;
                        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                        let mut staging = None;
                        gpu.device
                            .CreateTexture2D(&desc, None, Some(&mut staging))?;
                        let staging = staging.unwrap();
                        gpu.context.CopyResource(&staging, source);
                        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                        gpu.context
                            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                        let bytes = std::slice::from_raw_parts(
                            mapped.pData.cast::<u8>(),
                            mapped.RowPitch as usize * desc.Height as usize,
                        )
                        .to_vec();
                        gpu.context.Unmap(&staging, 0);
                        Ok(bytes)
                    };
                    let raced = compare(&copy.as_ref().unwrap().0)?;
                    let reference = compare(&texture)?;
                    checked += 1;
                    if raced != reference {
                        differing += 1;
                    }
                }
            }
            duplicate.ReleaseFrame()?;
            // The same signal with no frame held: our context's own
            // scheduling, without waiting for DWM.
            if info.LastPresentTime != 0 && mode == "plain" {
                let started = Instant::now();
                value += 1;
                context4.Signal(&fence11, value)?;
                gpu.context.Flush();
                while fence12.GetCompletedValue() < value {
                    std::hint::spin_loop();
                }
                plain.push(started.elapsed().as_secs_f64() * 1000.);
            }
        }
        stats("D3D11 signal holding no frame", &mut plain);
        if verify {
            println!("verify: {differing} of {checked} compute copies differ from the D3D11 copy");
        }
        println!("queue priority {} sync {sync}", compute.priority);
        stats("D3D11 signal after acquire", &mut signal);
        stats("compute copy", &mut copies);
        stats("acquire to copied", &mut total);
    }
    Ok(())
}
