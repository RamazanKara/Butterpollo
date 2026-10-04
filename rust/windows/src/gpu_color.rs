//! D3D11 conversion owned by the Rust host. No readback or vendor tone mapping.
use crate::capture::{Device, GpuImage, Pixel};
use anyhow::{Context, Result, bail};
use std::{borrow::Cow, sync::Arc};
use windows::{
    Win32::Graphics::{
        Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, Fxc::*},
        Direct3D11::*,
        Dxgi::Common::*,
    },
    core::PCSTR,
};

// Resize scRGB in linear light, convert Rec.709 to Rec.2020, apply absolute
// ST.2084 (1.0 scRGB = 80 nits), then subsample nonlinear RGB into limited YUV.
// R16/R16G16 render views address the two P010 planes; stored codes occupy the
// upper ten bits. Chroma covers four output pixels, including during scaling.
const SHADER: &str = include_str!("shaders/color.hlsl");
include!(concat!(env!("OUT_DIR"), "/shader_bytecode.rs"));

unsafe fn compile(entry: &'static [u8], target: &'static [u8]) -> Result<Cow<'static, [u8]>> {
    if let Some(bytecode) = shader_bytecode(entry, target) {
        return Ok(Cow::Borrowed(bytecode));
    }
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
            let detail = errors
                .map(|blob| {
                    String::from_utf8_lossy(std::slice::from_raw_parts(
                        blob.GetBufferPointer().cast(),
                        blob.GetBufferSize(),
                    ))
                    .into_owned()
                })
                .unwrap_or_default();
            bail!("HDR shader compilation: {error}: {detail}");
        }
        let code = code.context("shader compiler returned no bytecode")?;
        Ok(Cow::Owned(
            std::slice::from_raw_parts(code.GetBufferPointer().cast(), code.GetBufferSize())
                .to_vec(),
        ))
    }
}

struct Target {
    texture: Arc<ID3D11Texture2D>,
    luma: ID3D11RenderTargetView,
    chroma: Option<ID3D11RenderTargetView>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Nv12,
    P010,
    Ayuv,
    Planar444,
}
pub(crate) struct Converter {
    gpu: Device,
    vertex: ID3D11VertexShader,
    luma: ID3D11PixelShader,
    chroma: ID3D11PixelShader,
    constants: ID3D11Buffer,
    values: [u32; 20],
    dirty: bool,
    width: u32,
    height: u32,
    layout: Layout,
    targets: Vec<Target>,
}
/// `cbuffer Config` in color.hlsl, for a `source` (width, height, pixel)
/// converted to the stream, as both converters fill it.
pub(crate) fn constants(
    config: &butterpollo_core::rtsp::Negotiated,
    source: (u32, u32, Pixel),
    luminance: [f32; 2],
    pointer: Option<&crate::cursor::Cursor>,
) -> [u32; 20] {
    let mut values = [0; 20];
    values[..11].copy_from_slice(&[
        source.0,
        source.1,
        config.width,
        config.height,
        match source.2 {
            Pixel::Bgra8 => 0,
            Pixel::RgbaF16 => 1,
            Pixel::Rgba10Pq => 2,
        },
        u32::from(config.hdr),
        u32::from(config.color_matrix()),
        u32::from(config.full_range()),
        u32::from(config.ten_bit()),
        (luminance[0] / 80.).to_bits(),
        luminance[1].to_bits(),
    ]);
    values[12..].copy_from_slice(&pointer_constants(pointer));
    values
}
/// The pointer's part of `cbuffer Config`: position, size and blend mode.
fn pointer_constants(pointer: Option<&crate::cursor::Cursor>) -> [u32; 8] {
    pointer.map_or([0; 8], |c| {
        [
            c.position[0] as u32,
            c.position[1] as u32,
            c.width,
            c.height,
            if c.logic { 2 } else { 1 },
            0,
            0,
            0,
        ]
    })
}
impl Converter {
    pub fn new(
        gpu: &Device,
        config: &butterpollo_core::rtsp::Negotiated,
        source: (u32, u32, Pixel),
    ) -> Result<Self> {
        if config.width == 0
            || config.height == 0
            || (!config.yuv444
                && (!config.width.is_multiple_of(2) || !config.height.is_multiple_of(2)))
        {
            bail!("GPU conversion requires nonzero dimensions, and even dimensions for 4:2:0");
        }
        if !config.hdr && source.2 != Pixel::Bgra8 {
            bail!("HDR surface supplied to an SDR converter");
        }
        unsafe {
            let mut vertex = None;
            let mut luma = None;
            let mut chroma = None;
            gpu.device.CreateVertexShader(
                &compile(b"vertex\0", b"vs_5_0\0")?,
                None,
                Some(&mut vertex),
            )?;
            let layout = match (config.yuv444, config.ten_bit()) {
                (false, false) => Layout::Nv12,
                (false, true) => Layout::P010,
                (true, false) => Layout::Ayuv,
                (true, true) => Layout::Planar444,
            };
            gpu.device.CreatePixelShader(
                &compile(
                    match layout {
                        Layout::Ayuv => b"packed444\0",
                        Layout::Planar444 => b"planar444\0",
                        _ => b"luma\0",
                    },
                    b"ps_5_0\0",
                )?,
                None,
                Some(&mut luma),
            )?;
            gpu.device.CreatePixelShader(
                &compile(b"chroma\0", b"ps_5_0\0")?,
                None,
                Some(&mut chroma),
            )?;
            let values = constants(config, source, [100., 1.], None);
            let mut constants = None;
            gpu.device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 80,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    ..Default::default()
                },
                Some(&D3D11_SUBRESOURCE_DATA {
                    pSysMem: values.as_ptr().cast(),
                    ..Default::default()
                }),
                Some(&mut constants),
            )?;
            Ok(Self {
                gpu: gpu.clone(),
                vertex: vertex.unwrap(),
                luma: luma.unwrap(),
                chroma: chroma.unwrap(),
                constants: constants.unwrap(),
                values,
                dirty: false,
                width: config.width,
                height: config.height,
                layout,
                targets: vec![],
            })
        }
    }
    pub fn set_luminance(&mut self, luminance: [f32; 2]) {
        // Kept in step with `constants`.
        let white = (luminance[0] / 80.).to_bits();
        let scale = luminance[1].to_bits();
        if self.values[9] != white || self.values[10] != scale {
            self.values[9] = white;
            self.values[10] = scale;
            self.dirty = true;
        }
    }
    fn target(&mut self) -> Result<usize> {
        if let Some(index) = self
            .targets
            .iter()
            .position(|t| Arc::strong_count(&t.texture) == 1)
        {
            return Ok(index);
        }
        if self.targets.len() >= 8 {
            bail!("GPU conversion queue reached its bounded limit");
        }
        unsafe {
            let mut texture = None;
            self.gpu
                .device
                .CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Width: self.width,
                        Height: if self.layout == Layout::Planar444 {
                            self.height * 3
                        } else {
                            self.height
                        },
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: match self.layout {
                            Layout::Nv12 => DXGI_FORMAT_NV12,
                            Layout::P010 => DXGI_FORMAT_P010,
                            Layout::Ayuv => DXGI_FORMAT_AYUV,
                            Layout::Planar444 => DXGI_FORMAT_R16_UINT,
                        },
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0)
                            as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut texture),
                )
                .context("GPU YUV texture allocation")?;
            let texture = texture.unwrap();
            let view = |format| -> Result<ID3D11RenderTargetView> {
                let mut view = None;
                self.gpu.device.CreateRenderTargetView(
                    &texture,
                    Some(&D3D11_RENDER_TARGET_VIEW_DESC {
                        Format: format,
                        ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 },
                        },
                    }),
                    Some(&mut view),
                )?;
                Ok(view.unwrap())
            };
            let luma = view(match self.layout {
                Layout::Nv12 => DXGI_FORMAT_R8_UNORM,
                Layout::P010 => DXGI_FORMAT_R16_UNORM,
                Layout::Ayuv => DXGI_FORMAT_R8G8B8A8_UNORM,
                Layout::Planar444 => DXGI_FORMAT_R16_UINT,
            })?;
            let chroma = match self.layout {
                Layout::Nv12 => Some(view(DXGI_FORMAT_R8G8_UNORM)?),
                Layout::P010 => Some(view(DXGI_FORMAT_R16G16_UNORM)?),
                _ => None,
            };
            self.targets.push(Target {
                texture: Arc::new(texture),
                luma,
                chroma,
            });
            Ok(self.targets.len() - 1)
        }
    }
    fn source_views(&mut self, image: &GpuImage) -> Result<[Option<ID3D11ShaderResourceView>; 2]> {
        let pointer = image.cursor.as_ref();
        let values = pointer_constants(pointer);
        if self.values[12..] != values {
            self.values[12..].copy_from_slice(&values);
            self.dirty = true;
        }
        let mut source = None;
        unsafe {
            self.gpu.device.CreateShaderResourceView(
                image.texture.as_ref(),
                None,
                Some(&mut source),
            )?;
        }
        Ok([source, pointer.map(|c| c.view.clone())])
    }
    pub fn convert(&mut self, image: &GpuImage) -> Result<Arc<ID3D11Texture2D>> {
        let index = self.target()?;
        let source = self.source_views(image)?;
        unsafe {
            let context = &self.gpu.context;
            // Capture and the codec share this device. Keep the complete draw
            // together rather than merely protecting individual context calls.
            let lock: ID3D11Multithread = windows::core::Interface::cast(context)?;
            lock.Enter();
            if self.dirty {
                context.UpdateSubresource(
                    &self.constants,
                    0,
                    None,
                    self.values.as_ptr().cast(),
                    0,
                    0,
                );
                self.dirty = false;
            }
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(&self.vertex, None);
            context.PSSetShaderResources(0, Some(&source));
            context.PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            context.RSSetState(None);
            context.OMSetBlendState(None, None, u32::MAX);
            let target = &self.targets[index];
            for (view, shader, divisor) in std::iter::once((&target.luma, &self.luma, 1))
                .chain(target.chroma.as_ref().map(|view| (view, &self.chroma, 2)))
            {
                context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                    Width: (self.width / divisor) as f32,
                    Height: (self.height / divisor
                        * if self.layout == Layout::Planar444 {
                            3
                        } else {
                            1
                        }) as f32,
                    MinDepth: 0.,
                    MaxDepth: 1.,
                    ..Default::default()
                }]));
                context.OMSetRenderTargets(Some(&[Some(view.clone())]), None);
                context.PSSetShader(shader, None);
                context.Draw(3, 0);
            }
            context.PSSetShaderResources(0, Some(&[None, None]));
            context.OMSetRenderTargets(None, None);
            context.Flush();
            lock.Leave();
        }
        Ok(self.targets[index].texture.clone())
    }
}
pub(crate) struct PlanarConverter {
    base: Converter,
    shaders: [ID3D11PixelShader; 3],
    pub textures: [Arc<ID3D11Texture2D>; 3],
    views: [ID3D11RenderTargetView; 3],
    divisor: u32,
}
impl PlanarConverter {
    pub fn new(
        gpu: &Device,
        config: &butterpollo_core::rtsp::Negotiated,
        source: (u32, u32, Pixel),
    ) -> Result<Self> {
        let mut base = Converter::new(gpu, config, source)?;
        base.values[11] = u32::from(config.yuv444);
        base.dirty = true;
        let divisor = if config.yuv444 { 1 } else { 2 };
        unsafe {
            let mut textures = Vec::new();
            let mut views = Vec::new();
            let mut shaders = Vec::new();
            for (plane, entry) in [b"pyro_y\0", b"pyro_u\0", b"pyro_v\0"].iter().enumerate() {
                let mut shader = None;
                gpu.device.CreatePixelShader(
                    &compile(*entry, b"ps_5_0\0")?,
                    None,
                    Some(&mut shader),
                )?;
                shaders.push(shader.unwrap());
                let mut texture = None;
                gpu.device.CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Width: config.width / if plane == 0 { 1 } else { divisor },
                        Height: config.height / if plane == 0 { 1 } else { divisor },
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
                        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0)
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
                let mut view = None;
                gpu.device
                    .CreateRenderTargetView(&texture, None, Some(&mut view))?;
                textures.push(Arc::new(texture));
                views.push(view.unwrap());
            }
            Ok(Self {
                base,
                shaders: shaders.try_into().ok().unwrap(),
                textures: textures.try_into().ok().unwrap(),
                views: views.try_into().ok().unwrap(),
                divisor,
            })
        }
    }
    pub fn convert(&mut self, image: &GpuImage, luminance: [f32; 2]) -> Result<()> {
        self.base.set_luminance(luminance);
        let source = self.base.source_views(image)?;
        unsafe {
            let context = &self.base.gpu.context;
            let lock: ID3D11Multithread = windows::core::Interface::cast(context)?;
            lock.Enter();
            if self.base.dirty {
                context.UpdateSubresource(
                    &self.base.constants,
                    0,
                    None,
                    self.base.values.as_ptr().cast(),
                    0,
                    0,
                );
                self.base.dirty = false;
            }
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(&self.base.vertex, None);
            context.PSSetShaderResources(0, Some(&source));
            context.PSSetConstantBuffers(0, Some(&[Some(self.base.constants.clone())]));
            context.RSSetState(None);
            context.OMSetBlendState(None, None, u32::MAX);
            for plane in 0..3 {
                let divisor = if plane == 0 { 1 } else { self.divisor };
                context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                    Width: (self.base.width / divisor) as f32,
                    Height: (self.base.height / divisor) as f32,
                    MinDepth: 0.,
                    MaxDepth: 1.,
                    ..Default::default()
                }]));
                context.OMSetRenderTargets(Some(&[Some(self.views[plane].clone())]), None);
                context.PSSetShader(&self.shaders[plane], None);
                context.Draw(3, 0);
            }
            context.PSSetShaderResources(0, Some(&[None, None]));
            context.OMSetRenderTargets(None, None);
            lock.Leave();
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
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
                u16::from_le_bytes([reference[offset + c * 2], reference[offset + c * 2 + 1]])
                    as f32
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
}
