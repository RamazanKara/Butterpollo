use super::*;
use crate::capture::{ComGuard, Image};
use std::time::Instant;

#[test]
#[ignore = "requires D3D11 native P010 views and GPU timestamp queries"]
fn gpu_conversion_timing() -> Result<()> {
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let image = make_image(&[[0.; 3], [1.; 3], [12.5; 3], [125.; 3]], 492, 2184);
    let source = GpuImage::upload(&gpu, &image)?;
    let timer = crate::timing::Timer::new()?;
    let query = |kind| -> Result<ID3D11Query> {
        let mut query = None;
        unsafe {
            gpu.device.CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: kind,
                    MiscFlags: 0,
                },
                Some(&mut query),
            )?;
        }
        query.context("no timestamp query")
    };
    let mut reports = vec![];
    for (width, height) in [(1968, 2184), (3840, 2160)] {
        let config = butterpollo_core::rtsp::Negotiated {
            width,
            height,
            codec: 1,
            hdr: true,
            ..Default::default()
        };
        let preparation = Instant::now();
        let mut converter =
            Converter::new(&gpu, &config, (image.width, image.height, image.pixel))?;
        let preparation_ms = preparation.elapsed().as_secs_f64() * 1000.;
        for _ in 0..16 {
            converter.convert(&source)?;
        }
        let disjoint = query(D3D11_QUERY_TIMESTAMP_DISJOINT)?;
        let queries: Vec<_> = (0..64)
            .map(|_| Ok((query(D3D11_QUERY_TIMESTAMP)?, query(D3D11_QUERY_TIMESTAMP)?)))
            .collect::<Result<_>>()?;
        let mut calls = vec![];
        unsafe {
            gpu.context.Begin(&disjoint);
        }
        for (start, end) in &queries {
            unsafe {
                gpu.context.End(start);
            }
            let begin = Instant::now();
            converter.convert(&source)?;
            calls.push(begin.elapsed().as_secs_f64() * 1000.);
            unsafe {
                gpu.context.End(end);
            }
        }
        unsafe {
            gpu.context.End(&disjoint);
            gpu.context.Flush();
        }
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let mut clock = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
        loop {
            unsafe {
                gpu.context.GetData(
                    &disjoint,
                    Some((&mut clock as *mut D3D11_QUERY_DATA_TIMESTAMP_DISJOINT).cast()),
                    std::mem::size_of_val(&clock) as u32,
                    D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                )?;
            }
            if clock.Frequency != 0 {
                break;
            }
            if Instant::now() >= deadline {
                bail!("GPU timestamp clock timed out");
            }
            timer.until(Instant::now() + std::time::Duration::from_micros(250));
        }
        if clock.Disjoint.as_bool() {
            bail!("GPU clock changed during conversion benchmark");
        }
        let mut durations = vec![];
        for (start, end) in queries {
            let mut ticks = [0u64; 2];
            for (query, value) in [start, end].iter().zip(ticks.iter_mut()) {
                loop {
                    unsafe {
                        gpu.context.GetData(
                            query,
                            Some((value as *mut u64).cast()),
                            std::mem::size_of::<u64>() as u32,
                            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                        )?;
                    }
                    if *value != 0 {
                        break;
                    }
                    if Instant::now() >= deadline {
                        bail!("GPU timestamp result timed out");
                    }
                    timer.until(Instant::now() + std::time::Duration::from_micros(250));
                }
            }
            durations.push((ticks[1] - ticks[0]) as f64 * 1000. / clock.Frequency as f64);
        }
        let stats = |mut values: Vec<f64>| {
            values.sort_by(f64::total_cmp);
            serde_json::json!({"samples":values.len(),"mean_ms":values.iter().sum::<f64>() / values.len() as f64,"p95_ms":values[(values.len()-1)*95/100],"p99_ms":values[(values.len()-1)*99/100],"max_ms":values.last()})
        };
        reports.push(serde_json::json!({"width":width,"height":height,"preparation_ms":preparation_ms,"gpu":stats(durations),"cpu_call":stats(calls)}));
    }
    let report = serde_json::json!({"scope":"isolated FP16-to-P010 conversion, excludes capture, encoder, network and decoder", "source_width":image.width,"source_height":image.height,"adapter":gpu.display.adapter,"cases":reports});
    eprintln!("{report}");
    if let Some(path) = std::env::var_os("BUTTERPOLLO_TEST_GPU_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    Ok(())
}

fn make_image(colors: &[[f32; 3]], patch: usize, height: usize) -> Image {
    let width = colors.len() * patch;
    let mut bytes = vec![0; width * height * 8];
    for (index, pixel) in bytes.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        for (channel, value) in colors[(index % width) / patch].iter().enumerate() {
            pixel[channel * 2..channel * 2 + 2]
                .copy_from_slice(&half::f16::from_f32(*value).to_bits().to_le_bytes());
        }
        pixel[6..].copy_from_slice(&half::f16::ONE.to_bits().to_le_bytes());
    }
    Image {
        width: width as u32,
        height: height as u32,
        stride: width * 8,
        bytes,
        pixel: Pixel::RgbaF16,
        captured: Instant::now(),
    }
}
fn readback(gpu: &Device, texture: &ID3D11Texture2D) -> Result<Vec<u16>> {
    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut staging = None;
        gpu.device
            .CreateTexture2D(&desc, None, Some(&mut staging))?;
        let staging = staging.unwrap();
        gpu.context.CopyResource(&staging, texture);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let mut values = Vec::new();
        let rows = if desc.Format == DXGI_FORMAT_P010 {
            desc.Height as usize * 3 / 2
        } else {
            desc.Height as usize
        };
        for y in 0..rows {
            let row = std::slice::from_raw_parts(
                (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                desc.Width as usize * 2,
            );
            values.extend(
                row.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|v| u16::from_le_bytes(*v) >> 6),
            );
        }
        gpu.context.Unmap(&staging, 0);
        Ok(values)
    }
}
/// A P010 texture on the compute device, as `readback` gives it.
fn readback_compute(
    compute: &crate::compute::Compute,
    texture: &windows::Win32::Graphics::Direct3D12::ID3D12Resource,
) -> Result<Vec<u16>> {
    use windows::Win32::Graphics::Direct3D12::*;
    unsafe {
        let desc = texture.GetDesc();
        let mut layouts = [D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); 2];
        let mut total = 0u64;
        compute.device.GetCopyableFootprints(
            &desc,
            0,
            2,
            0,
            Some(layouts.as_mut_ptr()),
            None,
            None,
            Some(&mut total),
        );
        let mut buffer: Option<ID3D12Resource> = None;
        compute.device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_READBACK,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: total,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            },
            D3D12_RESOURCE_STATE_COPY_DEST,
            None,
            &mut buffer,
        )?;
        let buffer = buffer.unwrap();
        let allocator: ID3D12CommandAllocator = compute
            .device
            .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COMPUTE)?;
        let list: ID3D12GraphicsCommandList = compute.device.CreateCommandList(
            0,
            D3D12_COMMAND_LIST_TYPE_COMPUTE,
            &allocator,
            None,
        )?;
        for (plane, layout) in layouts.iter().enumerate() {
            let destination = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::ManuallyDrop::new(Some(buffer.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: *layout,
                },
            };
            let source = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::ManuallyDrop::new(Some(texture.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: plane as u32,
                },
            };
            list.CopyTextureRegion(&destination, 0, 0, 0, &source, None);
            drop(std::mem::ManuallyDrop::into_inner(destination.pResource));
            drop(std::mem::ManuallyDrop::into_inner(source.pResource));
        }
        list.Close()?;
        compute.wait(compute.execute(&list)?)?;
        let mut mapped = std::ptr::null_mut();
        buffer.Map(0, None, Some(&mut mapped))?;
        let mut values = Vec::new();
        for (plane, layout) in layouts.iter().enumerate() {
            let rows = if plane == 0 {
                desc.Height as usize
            } else {
                desc.Height as usize / 2
            };
            for y in 0..rows {
                let row = std::slice::from_raw_parts(
                    mapped
                        .cast::<u8>()
                        .add(layout.Offset as usize + y * layout.Footprint.RowPitch as usize),
                    desc.Width as usize * 2,
                );
                values.extend(
                    row.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|v| u16::from_le_bytes(*v) >> 6),
                );
            }
        }
        buffer.Unmap(0, None);
        Ok(values)
    }
}
#[test]
#[ignore = "requires D3D12 compute and native P010 conversion"]
fn compute_conversion_matches_the_graphics_converter() -> Result<()> {
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let compute = crate::compute::Compute::for_device(&gpu.device)?;
    let colors = [
        [0., 0., 0.],
        [1., 1., 1.],
        [12.5, 12.5, 12.5],
        [1.2, 0.1, 0.05],
        [0.05, 0.8, 0.3],
        [0.2, 0.3, 4.0],
    ];
    let hdr_image = GpuImage::upload(&gpu, &make_image(&colors, 16, 40))?;
    // Desktop BGRA: a gradient with distinct channels.
    let mut bytes = vec![0u8; 96 * 40 * 4];
    for (index, pixel) in bytes.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (index % 96, index / 96);
        *pixel = [(x * 2) as u8, (y * 6) as u8, (255 - x * 2) as u8, 255];
    }
    let sdr_image = GpuImage::upload(
        &gpu,
        &Image {
            width: 96,
            height: 40,
            stride: 96 * 4,
            bytes,
            pixel: Pixel::Bgra8,
            captured: Instant::now(),
        },
    )?;
    // Without a pointer, a monochrome (replace and XOR) pointer, and one
    // hanging off the left edge.
    let pointer = crate::cursor::Cursor::new(
        &gpu,
        &windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_POINTER_SHAPE_INFO {
            Type: windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0
                as u32,
            Width: 4,
            Height: 2,
            Pitch: 1,
            ..Default::default()
        },
        &[0b00110000, 0b01010000],
    )?;
    let mut placed = pointer.clone();
    placed.position = [20, 10];
    let mut clipped = pointer.clone();
    clipped.position = [-2, 30];
    // The same size, a downscale and a letterboxed shape.
    for (width, height) in [(96, 40), (48, 20), (64, 64)] {
        for (hdr, base) in [(true, &hdr_image), (false, &sdr_image)] {
            for cursor in [None, Some(&placed), Some(&clipped)] {
                let mut image = base.clone();
                image.cursor = cursor.cloned();
                let image = &image;
                let config = butterpollo_core::rtsp::Negotiated {
                    width,
                    height,
                    codec: 1,
                    hdr,
                    sdr_10bit: !hdr,
                    ..Default::default()
                };
                let mut graphics = Converter::new(&gpu, &config, (96, 40, image.pixel))?;
                graphics.set_luminance([100., 1.]);
                let expected = readback(&gpu, graphics.convert(image)?.as_ref())?;
                let mut converter =
                    crate::compute::Converter::new(compute.clone(), width, height, true)?;
                converter.values = constants(
                    &config,
                    (image.width, image.height, image.pixel),
                    [100., 1.],
                    image.cursor.as_ref(),
                );
                let shape = cursor.map(|c| converter.pointer(c)).transpose()?;
                let source = compute.open(&image.texture)?;
                let converted = converter.convert(
                    &source,
                    crate::compute::format(image.pixel),
                    shape.as_ref(),
                    None,
                )?;
                converted.wait()?;
                let actual = readback_compute(&compute, &converted.texture)?;
                assert_eq!(actual.len(), expected.len());
                for (index, (a, e)) in actual.iter().zip(&expected).enumerate() {
                    assert!(
                        a.abs_diff(*e) <= 1,
                        "{width}x{height} hdr {hdr} pointer {:?}: value {index} is {a}, the graphics converter gives {e}",
                        cursor.map(|c| c.position)
                    );
                }
            }
        }
    }
    Ok(())
}
/// A one-plane texture's bytes, row by row without padding.
fn plane_bytes(gpu: &Device, texture: &ID3D11Texture2D) -> Result<Vec<u8>> {
    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        let pixel = if desc.Format == DXGI_FORMAT_R16_UNORM {
            2
        } else {
            1
        };
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut staging = None;
        gpu.device
            .CreateTexture2D(&desc, None, Some(&mut staging))?;
        let staging = staging.unwrap();
        gpu.context.CopyResource(&staging, texture);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let mut bytes = Vec::new();
        for y in 0..desc.Height as usize {
            bytes.extend_from_slice(std::slice::from_raw_parts(
                (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                desc.Width as usize * pixel,
            ));
        }
        gpu.context.Unmap(&staging, 0);
        Ok(bytes)
    }
}
#[test]
#[ignore = "requires D3D12 compute"]
fn compute_planes_match_the_pyrowave_graphics_planes() -> Result<()> {
    use windows::Win32::Graphics::Direct3D12::{D3D12_FENCE_FLAG_NONE, ID3D12Fence};
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let compute = crate::compute::Compute::for_device(&gpu.device)?;
    let colors = [
        [0., 0., 0.],
        [1., 1., 1.],
        [12.5, 12.5, 12.5],
        [1.2, 0.1, 0.05],
        [0.05, 0.8, 0.3],
        [0.2, 0.3, 4.0],
    ];
    let hdr_image = GpuImage::upload(&gpu, &make_image(&colors, 16, 40))?;
    let mut bytes = vec![0u8; 96 * 40 * 4];
    for (index, pixel) in bytes.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (index % 96, index / 96);
        *pixel = [(x * 2) as u8, (y * 6) as u8, (255 - x * 2) as u8, 255];
    }
    let sdr_image = GpuImage::upload(
        &gpu,
        &Image {
            width: 96,
            height: 40,
            stride: 96 * 4,
            bytes,
            pixel: Pixel::Bgra8,
            captured: Instant::now(),
        },
    )?;
    let mut pointer = crate::cursor::Cursor::new(
        &gpu,
        &windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_POINTER_SHAPE_INFO {
            Type: windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0
                as u32,
            Width: 4,
            Height: 2,
            Pitch: 1,
            ..Default::default()
        },
        &[0b00110000, 0b01010000],
    )?;
    pointer.position = [20, 10];
    let fence: ID3D12Fence = unsafe { compute.device.CreateFence(0, D3D12_FENCE_FLAG_NONE)? };
    let mut value = 0;
    for (width, height) in [(96, 40), (48, 20), (64, 64)] {
        for yuv444 in [true, false] {
            for (hdr, base) in [(true, &hdr_image), (false, &sdr_image)] {
                for cursor in [None, Some(&pointer)] {
                    let mut image = base.clone();
                    image.cursor = cursor.cloned();
                    let config = butterpollo_core::rtsp::Negotiated {
                        width,
                        height,
                        codec: 3,
                        hdr,
                        yuv444,
                        ..Default::default()
                    };
                    let source = (image.width, image.height, image.pixel);
                    let mut graphics = PlanarConverter::new(&gpu, &config, source)?;
                    graphics.convert(&image, [100., 1.])?;
                    let expected = graphics
                        .textures
                        .iter()
                        .map(|plane| plane_bytes(&gpu, plane))
                        .collect::<Result<Vec<_>>>()?;
                    // Fresh planes filled with a sentinel, so nothing left
                    // from the graphics pass can match.
                    let outputs = PlanarConverter::new(&gpu, &config, source)?;
                    for plane in &outputs.textures {
                        let mut desc = D3D11_TEXTURE2D_DESC::default();
                        unsafe { plane.GetDesc(&mut desc) };
                        let pitch = desc.Width * if config.ten_bit() { 2 } else { 1 };
                        let fill = vec![0xab_u8; (pitch * desc.Height) as usize];
                        unsafe {
                            gpu.context.UpdateSubresource(
                                plane.as_ref(),
                                0,
                                None,
                                fill.as_ptr().cast(),
                                pitch,
                                0,
                            );
                            gpu.context.Flush();
                        }
                    }
                    let mut converter = crate::compute::Converter::new(
                        compute.clone(),
                        width,
                        height,
                        config.ten_bit(),
                    )?;
                    converter.values = constants(&config, source, [100., 1.], cursor);
                    converter.values[11] = u32::from(yuv444);
                    let shape = cursor.map(|c| converter.pointer(c)).transpose()?;
                    let planes = crate::compute::Planes {
                        planes: [
                            compute.open(&outputs.textures[0])?,
                            compute.open(&outputs.textures[1])?,
                            compute.open(&outputs.textures[2])?,
                        ],
                        format: if config.ten_bit() {
                            DXGI_FORMAT_R16_UNORM
                        } else {
                            DXGI_FORMAT_R8_UNORM
                        },
                        fence: fence.clone(),
                        after: value,
                        done: value + 1,
                    };
                    value += 1;
                    converter.convert_planes(
                        &compute.open(&image.texture)?,
                        crate::compute::format(image.pixel),
                        shape.as_ref(),
                        None,
                        &planes,
                    )?;
                    let deadline = Instant::now() + std::time::Duration::from_secs(2);
                    while unsafe { fence.GetCompletedValue() } < value {
                        assert!(
                            Instant::now() < deadline,
                            "the compute conversion did not finish"
                        );
                        std::thread::yield_now();
                    }
                    for (plane, (texture, expected)) in
                        outputs.textures.iter().zip(&expected).enumerate()
                    {
                        assert_eq!(
                            &plane_bytes(&gpu, texture)?,
                            expected,
                            "{width}x{height} 4:4:4 {yuv444} hdr {hdr} pointer {}: plane {plane}",
                            cursor.is_some()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
#[test]
#[ignore = "requires native D3D11 P010 conversion"]
fn gpu_desktop_pointer_matches_cpu_composition_for_sdr_and_hdr() -> Result<()> {
    use windows::Win32::Graphics::Dxgi::{
        DXGI_OUTDUPL_POINTER_SHAPE_INFO, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME,
    };
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let shape = DXGI_OUTDUPL_POINTER_SHAPE_INFO {
        Type: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32,
        Width: 4,
        Height: 2,
        Pitch: 1,
        ..Default::default()
    };
    let pointer = crate::cursor::Cursor::new(&gpu, &shape, &[0b00110000, 0b01010000])?;
    for pixel in [Pixel::Bgra8, Pixel::RgbaF16, Pixel::Rgba10Pq] {
        let mut source = if pixel == Pixel::RgbaF16 {
            make_image(&[[0.; 3]], 64, 64)
        } else {
            Image {
                width: 64,
                height: 64,
                stride: 256,
                bytes: vec![0; 64 * 256],
                pixel,
                captured: Instant::now(),
            }
        };
        let mut image = GpuImage::upload(&gpu, &source)?;
        image.cursor = Some(pointer.clone());
        pointer.blend(&mut source);
        let cpu_composited = GpuImage::upload(&gpu, &source)?;
        let config = butterpollo_core::rtsp::Negotiated {
            width: 64,
            height: 64,
            codec: 1,
            hdr: pixel != Pixel::Bgra8,
            sdr_10bit: pixel == Pixel::Bgra8,
            ..Default::default()
        };
        let mut converter = Converter::new(&gpu, &config, (64, 64, pixel))?;
        let actual = readback(&gpu, converter.convert(&image)?.as_ref())?;
        let expected = readback(&gpu, converter.convert(&cpu_composited)?.as_ref())?;
        for (a, e) in actual.iter().zip(&expected) {
            assert!(a.abs_diff(*e) <= 2, "pointer {pixel:?}: {a} versus {e}");
        }
        assert!(
            actual[1] > actual[0] + 300,
            "white replacement was not rendered"
        );
        assert!(actual[3] > actual[2] + 300, "XOR pointer was not rendered");
        // Removing or moving a cursor must not retain the previous constants.
        image.cursor.as_mut().unwrap().position = [-1, 1];
        let shifted = readback(&gpu, converter.convert(&image)?.as_ref())?;
        assert!(shifted[64] > shifted[0] + 300);
        image.cursor = None;
        let absent = readback(&gpu, converter.convert(&image)?.as_ref())?;
        assert!(
            absent
                .iter()
                .take(4096)
                .all(|value| value.abs_diff(64) <= 1)
        );
    }
    Ok(())
}
#[test]
#[ignore = "requires a D3D11 GPU with native P010 render views"]
fn gpu_sdr_ten_bit_respects_client_matrix_and_full_range() -> Result<()> {
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let image = Image {
        width: 64,
        height: 64,
        stride: 256,
        bytes: [0, 0, 255, 255].repeat(64 * 64),
        pixel: Pixel::Bgra8,
        captured: Instant::now(),
    };
    let uploaded = GpuImage::upload(&gpu, &image)?;
    for (csc_mode, expected) in [
        (0, [326, 361, 960]),
        (1, [306, 339, 1023]),
        (2, [250, 409, 960]),
        (3, [217, 395, 1023]),
    ] {
        let config = butterpollo_core::rtsp::Negotiated {
            width: 64,
            height: 64,
            codec: 1,
            sdr_10bit: true,
            csc_mode,
            ..Default::default()
        };
        let mut convert = Converter::new(&gpu, &config, (64, 64, Pixel::Bgra8))?;
        let output = readback(&gpu, convert.convert(&uploaded)?.as_ref())?;
        let observed = [output[0], output[4096], output[4097]];
        for (actual, wanted) in observed.into_iter().zip(expected) {
            assert!(actual.abs_diff(wanted) <= 1, "CSC {csc_mode}: {observed:?}");
        }
    }
    Ok(())
}
#[test]
#[ignore = "requires a D3D11 GPU with native P010 render views"]
fn gpu_hdr_preserves_absolute_luminance_gamut_and_linear_resize() -> Result<()> {
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let image = make_image(
        &[
            [0.; 3],
            [1.; 3],
            [12.5; 3],
            [125.; 3],
            [1., 0., 0.],
            [0., 1., 0.],
            [0., 0., 1.],
            [-0.5, 1., 0.5],
        ],
        64,
        64,
    );
    let config = butterpollo_core::rtsp::Negotiated {
        width: image.width,
        height: image.height,
        codec: 1,
        hdr: true,
        ..Default::default()
    };
    let mut convert = Converter::new(&gpu, &config, (image.width, image.height, image.pixel))?;
    let uploaded = GpuImage::upload(&gpu, &image)?;
    let output = readback(&gpu, convert.convert(&uploaded)?.as_ref())?;
    let mut reference = vec![];
    crate::color::hdr_rgba(&image, &mut reference);
    let width = image.width as usize;
    for patch in 0..8 {
        let x = patch * 64 + 32;
        let offset = (32 * width + x) * 8;
        let rgb: [f32; 3] = std::array::from_fn(|c| {
            u16::from_le_bytes([reference[offset + c * 2], reference[offset + c * 2 + 1]]) as f32
                / 65535.
        });
        let y = 0.2627 * rgb[0] + 0.6780 * rgb[1] + 0.0593 * rgb[2];
        let expected = [
            (64. + 876. * y).round(),
            (512. + 896. * (rgb[2] - y) / (2. * (1. - 0.0593))).round(),
            (512. + 896. * (rgb[0] - y) / (2. * (1. - 0.2627))).round(),
        ];
        let actual = [
            output[32 * width + x],
            output[width * image.height as usize + 16 * width + x],
            output[width * image.height as usize + 16 * width + x + 1],
        ];
        for c in 0..3 {
            assert!(
                (actual[c] as f32 - expected[c]).abs() <= 2.,
                "patch {patch} channel {c}: {} vs {}",
                actual[c],
                expected[c]
            );
        }
    }
    assert_eq!(output[32 * width + 32], 64);
    assert_eq!(output[32 * width + 3 * 64 + 32], 940);
    convert.set_luminance([100., 2.]);
    let expanded = readback(&gpu, convert.convert(&uploaded)?.as_ref())?;
    let expected = (64. + 876. * 0.82742465f32).round() as u16; // ST.2084 at 2000 nits
    assert!(expanded[32 * width + 2 * 64 + 32].abs_diff(expected) <= 2);
    let sdr = Image {
        width: 64,
        height: 64,
        stride: 256,
        bytes: vec![255; 64 * 256],
        captured: Instant::now(),
        pixel: Pixel::Bgra8,
    };
    let sdr = GpuImage::upload(&gpu, &sdr)?;
    let mut sdr_convert = Converter::new(
        &gpu,
        &butterpollo_core::rtsp::Negotiated {
            width: 64,
            height: 64,
            hdr: true,
            codec: 1,
            ..Default::default()
        },
        (64, 64, Pixel::Bgra8),
    )?;
    for (white, pq) in [(100., 0.5080784f32), (200., 0.5791332f32)] {
        sdr_convert.set_luminance([white, 1.]);
        let output = readback(&gpu, sdr_convert.convert(&sdr)?.as_ref())?;
        assert!(output[32 * 64 + 32].abs_diff((64. + 876. * pq).round() as u16) <= 2);
    }
    let resized = make_image(&[[0.; 3], [25.; 3], [0.; 3], [25.; 3]], 1, 4);
    let config = butterpollo_core::rtsp::Negotiated {
        width: 2,
        height: 2,
        codec: 1,
        hdr: true,
        ..Default::default()
    };
    let mut convert = Converter::new(
        &gpu,
        &config,
        (resized.width, resized.height, resized.pixel),
    )?;
    let output = readback(
        &gpu,
        convert
            .convert(&GpuImage::upload(&gpu, &resized)?)?
            .as_ref(),
    )?;
    for value in &output[..4] {
        assert!(value.abs_diff(723) <= 1, "1000-nit linear average: {value}");
    }
    // A wider source is letterboxed: an 8x2 image fills one row of 4x4.
    let wide = make_image(&[[125.; 3]], 8, 2);
    let mut convert = Converter::new(
        &gpu,
        &butterpollo_core::rtsp::Negotiated {
            width: 4,
            height: 4,
            codec: 1,
            hdr: true,
            ..Default::default()
        },
        (wide.width, wide.height, wide.pixel),
    )?;
    let output = readback(
        &gpu,
        convert.convert(&GpuImage::upload(&gpu, &wide)?)?.as_ref(),
    )?;
    for (row, picture) in [(0, false), (1, true), (2, false), (3, false)] {
        for value in &output[row * 4..row * 4 + 4] {
            assert_eq!(*value > 100, picture, "row {row}: {value}");
        }
    }
    Ok(())
}
#[test]
#[ignore = "requires a D3D11 GPU with native P010 render views"]
fn gpu_pool_does_not_overwrite_frames_held_by_the_codec() -> Result<()> {
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let black = make_image(&[[0.; 3]], 64, 64);
    let white = make_image(&[[125.; 3]], 64, 64);
    let config = butterpollo_core::rtsp::Negotiated {
        width: 64,
        height: 64,
        codec: 1,
        hdr: true,
        ..Default::default()
    };
    let mut convert = Converter::new(&gpu, &config, (64, 64, Pixel::RgbaF16))?;
    let black = GpuImage::upload(&gpu, &black)?;
    let white = GpuImage::upload(&gpu, &white)?;
    let first = convert.convert(&black)?;
    let mut held = vec![first.clone()];
    for _ in 0..7 {
        held.push(convert.convert(&white)?);
    }
    assert!(convert.convert(&white).is_err());
    assert!(readback(&gpu, &first)?[..64 * 64].iter().all(|y| *y == 64));
    held.pop();
    let next = convert.convert(&white)?;
    assert!(readback(&gpu, &next)?[..64 * 64].iter().all(|y| *y == 940));
    Ok(())
}
#[test]
#[ignore = "requires D3D11 RGBA and R16 render targets"]
fn gpu_444_preserves_per_pixel_chroma_and_planar_ten_bit_codes() -> Result<()> {
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    let image = Image {
        width: 64,
        height: 64,
        stride: 256,
        bytes: [0, 0, 255, 255, 255, 0, 0, 255].repeat(64 * 64 / 2),
        pixel: Pixel::Bgra8,
        captured: Instant::now(),
    };
    let source = GpuImage::upload(&gpu, &image)?;
    let config = butterpollo_core::rtsp::Negotiated {
        width: 64,
        height: 64,
        codec: 1,
        yuv444: true,
        csc_mode: 2,
        ..Default::default()
    };
    let mut converter = Converter::new(&gpu, &config, (64, 64, Pixel::Bgra8))?;
    // Test the packed shader through its documented RGBA-compatible view.
    // Radeon cannot allocate AYUV; native AYUV registration is exercised
    // separately by the opt-in NVIDIA hardware fixture.
    unsafe {
        let mut texture = None;
        gpu.device.CreateTexture2D(
            &D3D11_TEXTURE2D_DESC {
                Width: 64,
                Height: 64,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                ..Default::default()
            },
            None,
            Some(&mut texture),
        )?;
        let texture = texture.unwrap();
        let mut view = None;
        gpu.device
            .CreateRenderTargetView(&texture, None, Some(&mut view))?;
        converter.targets.push(Target {
            texture: Arc::new(texture),
            luma: view.unwrap(),
            chroma: None,
        });
    }
    let texture = converter.convert(&source)?;
    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        assert_eq!(desc.Format, DXGI_FORMAT_R8G8B8A8_UNORM);
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        let mut staging = None;
        gpu.device
            .CreateTexture2D(&desc, None, Some(&mut staging))?;
        let staging = staging.unwrap();
        gpu.context.CopyResource(&staging, texture.as_ref());
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let actual = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), 8).to_vec();
        gpu.context.Unmap(&staging, 0);
        // Adjacent red/blue pixels must retain separate chroma, V/U/Y/A.
        for (actual, expected) in actual.iter().zip([240u8, 102, 63, 255, 118, 240, 32, 255]) {
            assert!(actual.abs_diff(expected) <= 1, "AYUV {actual:?}");
        }
    }
    let config = butterpollo_core::rtsp::Negotiated {
        sdr_10bit: true,
        ..config
    };
    let mut converter = Converter::new(&gpu, &config, (64, 64, Pixel::Bgra8))?;
    let output = readback(&gpu, converter.convert(&source)?.as_ref())?;
    assert_eq!(output.len(), 64 * 64 * 3);
    let observed = [
        output[0],
        output[1],
        output[4096],
        output[4097],
        output[8192],
        output[8193],
    ];
    for (actual, expected) in observed.into_iter().zip([250u16, 127, 409, 960, 960, 471]) {
        assert!(actual.abs_diff(expected) <= 1, "planar 4:4:4 {observed:?}");
    }
    let image = make_image(&[[0.; 3], [1.; 3], [12.5; 3], [125.; 3]], 16, 64);
    let source = GpuImage::upload(&gpu, &image)?;
    let config = butterpollo_core::rtsp::Negotiated {
        hdr: true,
        ..config
    };
    let mut converter = Converter::new(&gpu, &config, (64, 64, Pixel::RgbaF16))?;
    let output = readback(&gpu, converter.convert(&source)?.as_ref())?;
    for (x, expected) in [64u16, 490, 723, 940].into_iter().enumerate() {
        assert!(output[x * 16 + 8].abs_diff(expected) <= 1);
        assert_eq!(output[4096 + x * 16 + 8], 512);
        assert_eq!(output[8192 + x * 16 + 8], 512);
    }
    Ok(())
}
