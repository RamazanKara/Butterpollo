use super::{Api, check};
use crate::{
    capture::{ComGuard, Device, GpuImage, Image, Pixel},
    encoder::Encoder,
    pyro_abi as p,
};
use anyhow::{Context, Result, ensure};
use butterpollo_core::{
    config::Config,
    packet::{PyrowaveFec, VideoPacketizer},
    pyrowave,
    rtsp::Negotiated,
};
use std::{collections::BTreeMap, ffi::c_void, ptr, sync::Arc, time::Instant};
use windows::{
    Win32::{
        Foundation::{CloseHandle, GENERIC_ALL},
        Graphics::{
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
    },
    core::{Interface, PCWSTR},
};

macro_rules! decoder_api {
    ($($name:ident: $ty:ty),* $(,)?) => {
        struct DecoderApi { $($name: $ty,)* }
        impl DecoderApi {
            unsafe fn load(dll: &libloading::Library) -> Result<Self> {
                Ok(Self { $($name: unsafe {
                    *dll.get::<$ty>(concat!("pyrowave_", stringify!($name), "\0").as_bytes())?
                },)* })
            }
        }
    };
}
decoder_api! {
    decoder_create: unsafe extern "C" fn(*const p::pyrowave_decoder_create_info, *mut p::pyrowave_decoder) -> p::pyrowave_result,
    decoder_destroy: unsafe extern "C" fn(p::pyrowave_decoder),
    decoder_clear: unsafe extern "C" fn(p::pyrowave_decoder),
    decoder_push_packet: unsafe extern "C" fn(p::pyrowave_decoder, *const c_void, usize) -> p::pyrowave_result,
    decoder_decode_is_ready: unsafe extern "C" fn(p::pyrowave_decoder, bool) -> bool,
    decoder_decode_gpu_buffer: unsafe extern "C" fn(p::pyrowave_decoder, *const p::pyrowave_gpu_sync_operation, *const p::pyrowave_gpu_sync_operation, *const p::pyrowave_gpu_buffers) -> p::pyrowave_result,
    sync_object_cpu_wait: unsafe extern "C" fn(p::pyrowave_sync_object, u64, u64) -> p::pyrowave_result,
}

struct Decoder {
    api: Arc<Api>,
    calls: DecoderApi,
    device: p::pyrowave_device,
    decoder: p::pyrowave_decoder,
    images: Vec<p::pyrowave_image>,
    textures: Vec<ID3D11Texture2D>,
    buffers: p::pyrowave_gpu_buffers,
    sync: p::pyrowave_sync_object,
    fence: Option<ID3D11Fence>,
    counter: u64,
}
impl Decoder {
    fn new(gpu: &Device, config: &Negotiated) -> Result<Self> {
        let api = Api::load()?;
        unsafe {
            let mut s = Self {
                calls: DecoderApi::load(&api._dll)?,
                api,
                device: ptr::null_mut(),
                decoder: ptr::null_mut(),
                images: vec![],
                textures: vec![],
                buffers: std::mem::zeroed(),
                sync: ptr::null_mut(),
                fence: None,
                counter: 0,
            };
            let desc = gpu.device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()?;
            let mut luid = p::pyrowave_luid { luid: [0; 8] };
            luid.luid[..4].copy_from_slice(&desc.AdapterLuid.LowPart.to_le_bytes());
            luid.luid[4..].copy_from_slice(&desc.AdapterLuid.HighPart.to_le_bytes());
            check((s.api.create_device_by_compat)(
                0,
                0,
                ptr::null(),
                ptr::null(),
                &luid,
                &mut s.device,
            ))?;
            check((s.calls.decoder_create)(
                &p::pyrowave_decoder_create_info {
                    device: s.device,
                    width: config.width as i32,
                    height: config.height as i32,
                    chroma: u32::from(config.yuv444),
                    fragment_path: false,
                },
                &mut s.decoder,
            ))?;
            for plane in 0..3 {
                let divisor = if plane == 0 || config.yuv444 { 1 } else { 2 };
                let (width, height) = (config.width / divisor, config.height / divisor);
                let mut texture = None;
                gpu.device.CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Width: width,
                        Height: height,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: if config.ten_bit() {
                            DXGI_FORMAT_R16_UNORM
                        } else {
                            DXGI_FORMAT_R8_UNORM
                        },
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_UNORDERED_ACCESS.0)
                            as u32,
                        MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0
                            | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0)
                            as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut texture),
                )?;
                let texture = texture.unwrap();
                let handle = texture.cast::<IDXGIResource1>()?.CreateSharedHandle(
                    None,
                    DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                    PCWSTR::null(),
                )?;
                let image_info = p::VkImageCreateInfo {
                    sType: p::VkStructureType_VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
                    imageType: p::VkImageType_VK_IMAGE_TYPE_2D,
                    format: if config.ten_bit() {
                        p::VkFormat_VK_FORMAT_R16_UNORM
                    } else {
                        p::VkFormat_VK_FORMAT_R8_UNORM
                    },
                    extent: p::VkExtent3D {
                        width,
                        height,
                        depth: 1,
                    },
                    mipLevels: 1,
                    arrayLayers: 1,
                    samples: p::VkSampleCountFlagBits_VK_SAMPLE_COUNT_1_BIT,
                    tiling: p::VkImageTiling_VK_IMAGE_TILING_OPTIMAL,
                    usage: p::VkImageUsageFlagBits_VK_IMAGE_USAGE_STORAGE_BIT
                        | p::VkImageUsageFlagBits_VK_IMAGE_USAGE_TRANSFER_SRC_BIT,
                    sharingMode: p::VkSharingMode_VK_SHARING_MODE_EXCLUSIVE,
                    ..std::mem::zeroed()
                };
                let mut image = ptr::null_mut();
                let result = (s.api.image_create)(&p::pyrowave_image_create_info {
                    device: s.device,
                    external_handle: handle.0 as usize,
                    handle_type: p::VkExternalMemoryHandleTypeFlagBits_VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_BIT,
                    image_create_info: &image_info,
                }, &mut image);
                if result != 0 {
                    let _ = CloseHandle(handle);
                    check(result).context("importing decoder output")?;
                }
                s.images.push(image);
                s.textures.push(texture);
                check((s.api.image_get_image_view)(
                    image,
                    p::VkImageAspectFlagBits_VK_IMAGE_ASPECT_COLOR_BIT,
                    p::VkImageUsageFlagBits_VK_IMAGE_USAGE_STORAGE_BIT,
                    &mut s.buffers.planes[plane],
                ))?;
            }
            gpu.device.cast::<ID3D11Device5>()?.CreateFence(
                0,
                D3D11_FENCE_FLAG_SHARED,
                &mut s.fence,
            )?;
            let handle = s.fence.as_ref().unwrap().CreateSharedHandle(
                None,
                GENERIC_ALL.0,
                PCWSTR::null(),
            )?;
            let result = (s.api.sync_object_create)(&p::pyrowave_sync_object_create_info {
                device: s.device,
                external_handle: handle.0 as usize,
                handle_type: p::VkExternalSemaphoreHandleTypeFlagBits_VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT,
                semaphore_type: p::VkSemaphoreType_VK_SEMAPHORE_TYPE_TIMELINE,
                import_flags: 0,
            }, &mut s.sync);
            if result != 0 {
                let _ = CloseHandle(handle);
                check(result)?;
            }
            Ok(s)
        }
    }

    fn decode(&mut self, gpu: &Device, packets: &[&[u8]]) -> Result<Vec<Vec<u16>>> {
        unsafe {
            (self.calls.decoder_clear)(self.decoder);
            ensure!(
                !(self.calls.decoder_decode_is_ready)(self.decoder, false),
                "empty decoder is ready"
            );
            for (index, packet) in packets.iter().enumerate() {
                check((self.calls.decoder_push_packet)(
                    self.decoder,
                    packet.as_ptr().cast(),
                    packet.len(),
                ))?;
                if index + 1 < packets.len() {
                    ensure!(
                        !(self.calls.decoder_decode_is_ready)(self.decoder, false),
                        "incomplete frame is ready"
                    );
                }
            }
            ensure!(
                (self.calls.decoder_decode_is_ready)(self.decoder, false),
                "frame is incomplete"
            );
            // Each preceding staging Map has completed. The old output can be discarded.
            let acquire_images: Vec<_> = self
                .images
                .iter()
                .map(|&image| p::pyrowave_gpu_external_reference {
                    image,
                    queue_family_index: u32::MAX,
                })
                .collect();
            let release_images: Vec<_> = self
                .images
                .iter()
                .map(|&image| p::pyrowave_gpu_external_reference {
                    image,
                    queue_family_index: u32::MAX - 1,
                })
                .collect();
            let acquire = p::pyrowave_gpu_sync_operation {
                images: acquire_images.as_ptr(),
                num_images: 3,
                sync: std::mem::zeroed(),
            };
            self.counter += 1;
            let release = p::pyrowave_gpu_sync_operation {
                images: release_images.as_ptr(),
                num_images: 3,
                sync: p::pyrowave_sync_point {
                    semaphore: (self.api.sync_object_get_semaphore)(self.sync),
                    value: self.counter,
                },
            };
            check((self.calls.decoder_decode_gpu_buffer)(
                self.decoder,
                &acquire,
                &release,
                &self.buffers,
            ))?;
            check((self.calls.sync_object_cpu_wait)(
                self.sync,
                self.counter,
                10_000_000_000,
            ))
            .context("waiting for decode")?;
            gpu.context
                .cast::<ID3D11DeviceContext4>()?
                .Wait(self.fence.as_ref().unwrap(), self.counter)?;
        }
        self.textures
            .iter()
            .map(|texture| readback(gpu, texture))
            .collect()
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            if !self.decoder.is_null() {
                (self.calls.decoder_destroy)(self.decoder);
            }
            for image in &self.images {
                (self.api.image_destroy)(*image);
            }
            if !self.sync.is_null() {
                (self.api.sync_object_destroy)(self.sync);
            }
            if !self.device.is_null() {
                (self.api.device_destroy)(self.device);
            }
        }
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
        let mut values = Vec::with_capacity((desc.Width * desc.Height) as usize);
        let sixteen = desc.Format == DXGI_FORMAT_R16_UNORM;
        for y in 0..desc.Height as usize {
            let row = std::slice::from_raw_parts(
                (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                desc.Width as usize * if sixteen { 2 } else { 1 },
            );
            if sixteen {
                values.extend(
                    row.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|v| u16::from_le_bytes(*v)),
                );
            } else {
                values.extend(row.iter().map(|&v| u16::from(v)));
            }
        }
        gpu.context.Unmap(&staging, 0);
        Ok(values)
    }
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

// A lossless client receive: order data shards, remove the short frame header,
// and trim the final shard. This deliberately does not use the host's layout parser.
fn receive(packets: &[Vec<u8>]) -> Result<Vec<u8>> {
    let mut data = BTreeMap::new();
    let mut counts = BTreeMap::new();
    for packet in packets.iter().rev() {
        ensure!(
            packet.len() >= 40 && packet[0] == 0x90,
            "invalid RTP packet"
        );
        let block = (packet[27] >> 4) & 3;
        let info = word(packet, 28);
        let (count, index) = (info >> 22, (info >> 12) & 1023);
        counts.insert(block, count);
        if index < count {
            ensure!(
                data.insert((block, index), &packet[32..]).is_none(),
                "duplicate data shard"
            );
        }
    }
    ensure!(
        counts.len() == usize::from(packets[0][27] >> 6) + 1,
        "missing FEC block"
    );
    let mut joined = Vec::new();
    for (block, count) in counts {
        for index in 0..count {
            joined.extend_from_slice(data.get(&(block, index)).context("missing data shard")?);
        }
    }
    let last = usize::from(u16::from_le_bytes(joined[4..6].try_into().unwrap()));
    let shard_size = packets[0].len() - 32;
    ensure!(
        joined[0] == 1 && joined[3] == 2 && (1..=shard_size).contains(&last),
        "invalid frame header"
    );
    joined.truncate(joined.len() - shard_size + last);
    Ok(joined[8..].to_vec())
}

fn codec_packets(frame: &[u8], records: bool) -> Result<Vec<&[u8]>> {
    let mut packets = Vec::new();
    let mut offset = if records { 0 } else { 4 };
    while offset < frame.len() {
        ensure!(frame.len() - offset >= 8, "short codec header");
        let header = word(frame, offset);
        let size = if records {
            if header == u32::MAX {
                8 + word(frame, offset + 4) as usize * 4
            } else if header & 0x80000000 != 0 {
                8
            } else {
                ((header >> 16) & 0xfff) as usize * 4
            }
        } else {
            offset += 4;
            header as usize
        };
        ensure!(
            size >= 8 && size <= frame.len() - offset,
            "invalid codec packet length"
        );
        if !records || header != u32::MAX {
            packets.push(&frame[offset..offset + size]);
        }
        offset += size;
    }
    ensure!(
        records || packets.len() == word(frame, 0) as usize,
        "packet count mismatch"
    );
    Ok(packets)
}

fn linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn pq2020(rgb: [f64; 3]) -> [f64; 3] {
    let matrix = [
        [0.627404, 0.329283, 0.043313],
        [0.069097, 0.919540, 0.011362],
        [0.016391, 0.088013, 0.895595],
    ];
    matrix.map(|row| {
        let nits = row.iter().zip(rgb).map(|(k, v)| k * v * 80.).sum::<f64>();
        let p = (nits / 10000.).clamp(0., 1.).powf(2610. / 16384.);
        ((3424. / 4096. + 2413. / 128. * p) / (1. + 2392. / 128. * p)).powf(2523. / 32.)
    })
}

fn chart(x: u32, y: u32, width: u32, height: u32, phase: u32) -> [f64; 3] {
    let (half_width, half_height) = (width / 2, height / 2);
    if y < half_height && x < half_width {
        let bar = ((x * 8 / half_width + phase) % 8) as usize;
        return [
            [1., 1., 1.],
            [1., 1., 0.],
            [0., 1., 1.],
            [0., 1., 0.],
            [1., 0., 1.],
            [1., 0., 0.],
            [0., 0., 1.],
            [0., 0., 0.],
        ][bar];
    }
    if y < half_height {
        let v = f64::from(x - half_width) / f64::from(half_width - 1);
        return match y * 3 / half_height {
            0 => [v; 3],
            1 => [v, 1. - v, 0.25],
            _ => [0.25, v, 1. - v],
        };
    }
    if x < half_width {
        // 5x7 PYROWAVE glyphs at one and two pixels per stroke.
        let glyphs = [
            [30, 17, 17, 30, 16, 16, 16],
            [17, 17, 10, 4, 4, 4, 4],
            [30, 17, 17, 30, 20, 18, 17],
            [14, 17, 17, 17, 17, 17, 14],
            [17, 17, 17, 21, 21, 21, 10],
            [14, 17, 17, 31, 17, 17, 17],
            [17, 17, 17, 17, 17, 10, 4],
            [31, 16, 16, 30, 16, 16, 31],
        ];
        let scale = if y < height * 3 / 4 { 1 } else { 2 };
        let (tx, ty) = ((x + phase * 3) / scale, (y - half_height) / scale);
        let ink = tx % 6 < 5
            && ty % 10 < 7
            && glyphs[(tx / 6 % 8) as usize][(ty % 10) as usize] & (1 << (4 - tx % 6)) != 0;
        return if ink {
            [0.92, 0.72, 0.25]
        } else {
            [0.06, 0.10, 0.16]
        };
    }
    let mut state = x.wrapping_mul(747796405)
        ^ y.wrapping_mul(2891336453)
        ^ (phase + 1).wrapping_mul(277803737);
    std::array::from_fn(|_| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        0.25 + f64::from(state & 65535) / 65535. * 0.5
    })
}

fn picture(config: &Negotiated, phase: u32) -> (Image, Vec<Vec<u16>>) {
    let (width, height) = (config.width, config.height);
    let pixel = if !config.ten_bit() {
        Pixel::Bgra8
    } else if config.hdr && phase == 1 {
        Pixel::Rgba10Pq
    } else {
        Pixel::RgbaF16
    };
    let stride = width as usize * if pixel == Pixel::RgbaF16 { 8 } else { 4 };
    let mut bytes = Vec::with_capacity(stride * height as usize);
    let mut source = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let rgb = chart(x, y, width, height, phase);
            let rgb = if pixel == Pixel::Bgra8 {
                let codes = rgb.map(|v| (v * 255.).round() as u8);
                bytes.extend_from_slice(&[codes[2], codes[1], codes[0], 255]);
                codes.map(|v| f64::from(v) / 255.)
            } else {
                let sc = rgb.map(|v| linear(v) * if config.hdr { 12.5 } else { 1.25 });
                if pixel == Pixel::Rgba10Pq {
                    let codes = pq2020(sc).map(|v| (v * 1023.).round() as u32);
                    bytes.extend_from_slice(
                        &(codes[0] | (codes[1] << 10) | (codes[2] << 20) | (3 << 30)).to_le_bytes(),
                    );
                    codes.map(|v| f64::from(v) / 1023.)
                } else {
                    let sc = sc.map(half::f16::from_f64);
                    for v in sc {
                        bytes.extend_from_slice(&v.to_le_bytes());
                    }
                    bytes.extend_from_slice(&half::f16::ONE.to_le_bytes());
                    let sc = sc.map(half::f16::to_f64);
                    if config.hdr {
                        pq2020(sc)
                    } else {
                        sc.map(|v| {
                            let v = (v / 1.25).clamp(0., 1.);
                            if v <= 0.0031308 {
                                12.92 * v
                            } else {
                                1.055 * v.powf(1. / 2.4) - 0.055
                            }
                        })
                    }
                }
            };
            source.push(rgb);
        }
    }
    let peak = if config.ten_bit() { 1023. } else { 255. };
    let k = if config.hdr {
        [0.2627, 0.6780, 0.0593]
    } else {
        [0.2126, 0.7152, 0.0722]
    };
    let yuv = |rgb: [f64; 3]| {
        let y = k.iter().zip(rgb).map(|(k, v)| k * v).sum::<f64>();
        [
            y,
            (rgb[2] - y) / (2. * (1. - k[2])),
            (rgb[0] - y) / (2. * (1. - k[0])),
        ]
    };
    let mut planes = vec![Vec::new(), Vec::new(), Vec::new()];
    for rgb in &source {
        planes[0].push((yuv(*rgb)[0] * peak).round().clamp(0., peak) as u16);
    }
    let divisor = if config.yuv444 { 1 } else { 2 };
    for y in (0..height).step_by(divisor) {
        for x in (0..width).step_by(divisor) {
            let mut rgb = [0.; 3];
            for dy in 0..divisor {
                for dx in 0..divisor {
                    let sample = source[(y as usize + dy) * width as usize + x as usize + dx];
                    for c in 0..3 {
                        rgb[c] += sample[c] / (divisor * divisor) as f64;
                    }
                }
            }
            let values = yuv(rgb);
            for c in 1..3 {
                planes[c].push(
                    ((peak + 1.) / 2. + peak * values[c])
                        .round()
                        .clamp(0., peak) as u16,
                );
            }
        }
    }
    (
        Image {
            width,
            height,
            stride,
            bytes,
            captured: Instant::now(),
            pixel,
        },
        planes,
    )
}

#[derive(serde::Serialize)]
struct Error {
    psnr: f64,
    max: f64,
}
fn compare(decoded: &[u16], expected: &[u16], ten_bit: bool, width: usize) -> [Error; 5] {
    assert_eq!(decoded.len(), expected.len());
    let peak = if ten_bit { 1023. } else { 255. };
    let scale = if ten_bit { peak / 65535. } else { 1. };
    let mut sums = [(0., 0_f64, 0); 5];
    for (index, (&actual, &expected)) in decoded.iter().zip(expected).enumerate() {
        let error = (f64::from(actual) * scale - f64::from(expected)).abs();
        let panel = 1
            + usize::from(index % width >= width / 2)
            + 2 * usize::from(index >= decoded.len() / 2);
        for at in [0, panel] {
            sums[at].0 += error * error;
            sums[at].1 = sums[at].1.max(error);
            sums[at].2 += 1;
        }
    }
    sums.map(|(squares, maximum, count)| Error {
        psnr: if squares == 0. {
            f64::INFINITY
        } else {
            10. * (peak * peak * count as f64 / squares).log10()
        },
        max: maximum,
    })
}

fn stream_status() -> Result<String> {
    let log =
        match std::fs::read_to_string(r"C:\ProgramData\Butterpollo\config\logs\butterpollo.log") {
            Ok(log) => log,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok("installed host log absent; stream activity unknown".into());
            }
            Err(error) => return Err(error.into()),
        };
    Ok(log
        .lines()
        .rev()
        .find(|line| line.contains("CLIENT CONNECTED") || line.contains("CLIENT DISCONNECTED"))
        .unwrap_or("no client events in current log")
        .to_owned())
}

#[test]
#[ignore = "requires AMD D3D12 compute, Vulkan and the pinned PyroWave DLL; see rust/PERFORMANCE.md"]
fn pyrowave_decoded_end_to_end() -> Result<()> {
    let _com = ComGuard::new()?;
    let gpu = Device::new("")?;
    ensure!(
        crate::compute::copies_on(&gpu.device),
        "this matrix requires an AMD adapter"
    );
    println!(
        "PYROWAVE_ENV {}",
        serde_json::json!({"adapter":gpu.display.adapter,"bitstream":pyrowave::BITSTREAM_ID,"panel_order":["bars","gradient","text","noise"]})
    );
    for (width, height) in [(1920, 1080), (1280, 720)] {
        for (format, hdr, sdr_10bit) in [
            ("sdr8", false, false),
            ("sdr10", false, true),
            ("hdr10", true, false),
        ] {
            for yuv444 in [false, true] {
                let mut config = Negotiated {
                    width,
                    height,
                    codec: 3,
                    hdr,
                    sdr_10bit,
                    yuv444,
                    csc_mode: 3,
                    packet_size: 1392,
                    fps: 60,
                    ..Default::default()
                };
                let mut decoder = Decoder::new(&gpu, &config)?;
                let pictures = (0..2)
                    .map(|phase| {
                        let (source, reference) = picture(&config, phase);
                        Ok((GpuImage::upload(&gpu, &source)?, reference))
                    })
                    .collect::<Result<Vec<_>>>()?;
                for kbps in [1_000_000, 400_000, 125_000, 30_000] {
                    config.bitrate_kbps = kbps;
                    let mut baseline: Vec<Option<Vec<Vec<u16>>>> = vec![None, None];
                    for records in [false, true] {
                        config.pyrowave_records = records;
                        let mut encoders = [false, true]
                            .into_iter()
                            .map(|compute| {
                                Encoder::new_gpu_options(
                                    &config,
                                    "auto",
                                    &pictures[0].0,
                                    &Config::parse(&format!("gpu_compute_conversion = {compute}"))?,
                                )
                            })
                            .collect::<Result<Vec<_>>>()?;
                        for batch in 0..3 {
                            let status = stream_status()?;
                            println!(
                                "PYROWAVE_BATCH {}",
                                serde_json::json!({"width":width,"height":height,"format":format,"chroma":if yuv444 {444} else {420},"kbps":kbps,"records":records,"batch":batch,"stream":status})
                            );
                            for compute in if batch % 2 == 0 {
                                [false, true]
                            } else {
                                [true, false]
                            } {
                                let encoder = &mut encoders[usize::from(compute)];
                                for (phase, (source, reference)) in pictures.iter().enumerate() {
                                    // Use the normal first-frame budget for every sample. CPU reference
                                    // and readback time must not increase the plain-packet bitrate budget.
                                    let Encoder::Pyrowave(native) = encoder else {
                                        unreachable!()
                                    };
                                    native.previous = None;
                                    let frames = encoder.encode_gpu(source, true, kbps)?;
                                    ensure!(frames.len() == 1, "encoder dropped the frame");
                                    let Encoder::Pyrowave(native) = encoder else {
                                        unreachable!()
                                    };
                                    ensure!(
                                        (native.compute.is_some()
                                            && crate::compute::shareable(&source.texture))
                                            == compute,
                                        "requested conversion path was not used"
                                    );
                                    let mut wire = VideoPacketizer {
                                        sequence: 0,
                                        iv_counter: 0,
                                        frame: 1,
                                        packet_size: config.packet_size,
                                        fec_percent: 0,
                                        min_fec: 2,
                                        key: None,
                                    };
                                    let packets = wire.encode_pyrowave(
                                        &frames[0].bytes,
                                        9000,
                                        500,
                                        PyrowaveFec {
                                            records,
                                            critical_percentage: 20,
                                            detail_percentage: 0,
                                            wire_budget: 0,
                                            ipv6: false,
                                        },
                                    )?;
                                    let frame = receive(&packets)?;
                                    ensure!(
                                        frame == frames[0].bytes,
                                        "wire reconstruction changed the frame"
                                    );
                                    let decoded =
                                        decoder.decode(&gpu, &codec_packets(&frame, records)?)?;
                                    if let Some(baseline) = &baseline[phase] {
                                        for plane in 0..3 {
                                            let difference = decoded[plane]
                                                .iter()
                                                .zip(&baseline[plane])
                                                .map(|(a, b)| a.abs_diff(*b))
                                                .max()
                                                .unwrap();
                                            ensure!(
                                                difference == 0,
                                                "decoded pixels differ by {difference} storage units: {format} {width}x{height} 444={yuv444} {kbps} records={records} compute={compute} batch={batch} phase={phase} plane={plane}"
                                            );
                                        }
                                    }
                                    let errors: Vec<_> = decoded
                                        .iter()
                                        .zip(reference)
                                        .enumerate()
                                        .map(|(plane, (a, b))| {
                                            compare(
                                                a,
                                                b,
                                                config.ten_bit(),
                                                width as usize
                                                    / if plane == 0 || yuv444 { 1 } else { 2 },
                                            )
                                        })
                                        .collect();
                                    println!(
                                        "PYROWAVE_RESULT {}",
                                        serde_json::json!({"width":width,"height":height,"format":format,"chroma":if yuv444 {444} else {420},"kbps":kbps,"minimum_kbps":pyrowave::minimum_kbps(width,height,60000),"records":records,"compute":compute,"batch":batch,"phase":phase,"source":format!("{:?}",source.pixel),"bytes":frame.len(),"datagrams":packets.len(),"planes":errors.iter().map(|e| &e[0]).collect::<Vec<_>>(),"panels":errors.iter().map(|e| &e[1..]).collect::<Vec<_>>()})
                                    );
                                    if kbps == 1_000_000 {
                                        ensure!(
                                            errors.iter().all(|e| e[0].psnr >= 40.
                                                && e[1..].iter().all(|panel| panel.psnr >= 35.)),
                                            "high-bitrate control PSNR below 40 dB per plane or 35 dB per panel"
                                        );
                                    } else if kbps == 400_000 {
                                        ensure!(
                                            errors.iter().all(|e| e[0].psnr >= 24.),
                                            "400 Mbps PSNR below 24 dB per plane"
                                        );
                                    }
                                    if baseline[phase].is_none() {
                                        baseline[phase] = Some(decoded);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
