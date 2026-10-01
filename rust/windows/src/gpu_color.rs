//! D3D11 conversion owned by the Rust host. No readback or vendor tone mapping.
use crate::capture::{Device, GpuImage, Pixel};
use anyhow::{Context, Result, bail};
use std::sync::Arc;
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
const SHADER: &str = r#"
Texture2D<float4> source : register(t0);
cbuffer Config : register(b0) {
    uint2 sourceSize; uint2 targetSize;
    uint pixel; uint hdr; uint colorMatrix; uint fullRange;
    uint tenBit; float sdrWhiteScale; float hdrScale; uint padding;
};
float4 vertex(uint id : SV_VertexID) : SV_Position {
    float2 p = float2((id << 1) & 2, id & 2);
    return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
}
float3 load(int2 p) {
    float3 rgb = source.Load(int3(clamp(p, int2(0, 0), int2(sourceSize) - 1), 0)).rgb;
    if (hdr != 0 && pixel == 0) {
        rgb = lerp(pow((rgb + 0.055) / 1.055, 2.4), rgb / 12.92, step(rgb, 0.04045));
        rgb *= sdrWhiteScale;
    }
    if (hdr != 0 && pixel == 1) rgb *= hdrScale;
    return rgb;
}
float3 nonlinear(float2 target) {
    float2 p = (target + 0.5) * float2(sourceSize) / float2(targetSize) - 0.5;
    int2 at = int2(floor(p));
    float2 f = frac(p);
    float3 rgb = lerp(lerp(load(at), load(at + int2(1, 0)), f.x),
                      lerp(load(at + int2(0, 1)), load(at + int2(1, 1)), f.x), f.y);
    if (hdr == 0 || pixel == 2) return rgb;
    rgb = float3(dot(rgb, float3(0.627404, 0.329283, 0.043313)),
                 dot(rgb, float3(0.069097, 0.919540, 0.011362)),
                 dot(rgb, float3(0.016391, 0.088013, 0.895595)));
    float3 luminance = clamp(rgb / 125.0, 0.0, 1.0);
    float3 power = pow(luminance, 2610.0 / 16384.0);
    return pow((3424.0 / 4096.0 + 2413.0 / 128.0 * power) /
               (1.0 + 2392.0 / 128.0 * power), 2523.0 / 32.0);
}
float3 weights() { return colorMatrix == 2 ? float3(0.2627, 0.6780, 0.0593) : colorMatrix == 0 ? float3(0.299, 0.587, 0.114) : float3(0.2126, 0.7152, 0.0722); }
float4 luma(float4 p : SV_Position) : SV_Target {
    float y = dot(nonlinear(p.xy - 0.5), weights());
    float code = tenBit != 0 ? floor(clamp((fullRange != 0 ? 1023 * y : 64 + 876 * y), 0, 1023) + 0.5) * 64 / 65535.0
                            : floor(clamp((fullRange != 0 ? 255 * y : 16 + 219 * y), 0, 255) + 0.5) / 255.0;
    return float4(code, 0, 0, 1);
}
float4 chroma(float4 p : SV_Position) : SV_Target {
    float2 at = floor(p.xy) * 2;
    float3 rgb = (nonlinear(at) + nonlinear(at + float2(1, 0)) +
                  nonlinear(at + float2(0, 1)) + nonlinear(at + float2(1, 1))) * 0.25;
    float3 k = weights();
    float y = dot(rgb, k);
    float2 uv = float2((rgb.b - y) / (2 * (1 - k.b)), (rgb.r - y) / (2 * (1 - k.r)));
    float2 code = tenBit != 0 ? floor(clamp(512 + (fullRange != 0 ? 1023 : 896) * uv, 0, 1023) + 0.5) * 64 / 65535.0
                             : floor(clamp(128 + (fullRange != 0 ? 255 : 224) * uv, 0, 255) + 0.5) / 255.0;
    return float4(code, 0, 1);
}
float3 yuv444(float2 p) {
    float3 rgb = nonlinear(p);
    float3 k = weights();
    float y = dot(rgb, k);
    return float3(y, (rgb.b - y) / (2 * (1 - k.b)), (rgb.r - y) / (2 * (1 - k.r)));
}
// DXGI AYUV has V, U, Y, A byte order in the compatible RGBA render view.
float4 packed444(float4 p : SV_Position) : SV_Target {
    float3 yuv = yuv444(p.xy - 0.5);
    float y = floor(clamp(fullRange != 0 ? 255 * yuv.x : 16 + 219 * yuv.x, 0, 255) + 0.5);
    float2 uv = floor(clamp(128 + (fullRange != 0 ? 255 : 224) * yuv.yz, 0, 255) + 0.5);
    return float4(uv.y, uv.x, y, 255) / 255;
}
// CUDA imports this single R16_UINT texture into pitched Y/U/V device memory.
// NVENC's 16-bit 4:4:4 container stores 10-bit codes in the upper bits.
uint planar444(float4 p : SV_Position) : SV_Target {
    uint plane = uint(p.y) / targetSize.y;
    float3 yuv = yuv444(float2(p.x - 0.5, p.y - 0.5 - plane * targetSize.y));
    float code = plane == 0 ? (fullRange != 0 ? 1023 * yuv.x : 64 + 876 * yuv.x)
                           : 512 + (fullRange != 0 ? 1023 : 896) * yuv[plane];
    return uint(floor(clamp(code, 0, 1023) + 0.5)) << 6;
}
float4 pyro_y(float4 p : SV_Position) : SV_Target {
    float y = yuv444(p.xy - 0.5).x;
    float maximum = tenBit != 0 ? 1023 : 255;
    float code = tenBit != 0 ? (fullRange != 0 ? 1023*y : 64+876*y) : (fullRange != 0 ? 255*y : 16+219*y);
    return float4(floor(clamp(code,0,maximum)+0.5)/maximum,0,0,1);
}
float2 pyro_chroma(float2 p) {
    float2 at = p - 0.5;
    float3 yuv;
    if (padding != 0) yuv = yuv444(at);
    else {
        at = floor(p)*2;
        float3 rgb = (nonlinear(at)+nonlinear(at+float2(1,0))+nonlinear(at+float2(0,1))+nonlinear(at+float2(1,1)))*0.25;
        float3 k = weights(); float y = dot(rgb,k);
        yuv = float3(y,(rgb.b-y)/(2*(1-k.b)),(rgb.r-y)/(2*(1-k.r)));
    }
    float maximum = tenBit != 0 ? 1023 : 255;
    float2 code = tenBit != 0 ? 512+(fullRange != 0 ? 1023 : 896)*yuv.yz : 128+(fullRange != 0 ? 255 : 224)*yuv.yz;
    return floor(clamp(code,0,maximum)+0.5)/maximum;
}
float4 pyro_u(float4 p : SV_Position) : SV_Target { return float4(pyro_chroma(p.xy).x,0,0,1); }
float4 pyro_v(float4 p : SV_Position) : SV_Target { return float4(pyro_chroma(p.xy).y,0,0,1); }
"#;

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
        Ok(
            std::slice::from_raw_parts(code.GetBufferPointer().cast(), code.GetBufferSize())
                .to_vec(),
        )
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
    values: [u32; 12],
    dirty: bool,
    width: u32,
    height: u32,
    layout: Layout,
    targets: Vec<Target>,
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
            let values = [
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
                1.25f32.to_bits(),
                1.0f32.to_bits(),
                0,
            ];
            let mut constants = None;
            gpu.device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 48,
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
    pub fn convert(&mut self, image: &GpuImage) -> Result<Arc<ID3D11Texture2D>> {
        let index = self.target()?;
        unsafe {
            let mut source = None;
            self.gpu.device.CreateShaderResourceView(
                image.texture.as_ref(),
                None,
                Some(&mut source),
            )?;
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
            context.PSSetShaderResources(0, Some(&[source]));
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
            context.PSSetShaderResources(0, Some(&[None]));
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
        unsafe {
            let mut source = None;
            self.base.gpu.device.CreateShaderResourceView(
                image.texture.as_ref(),
                None,
                Some(&mut source),
            )?;
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
            context.PSSetShaderResources(0, Some(&[source]));
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
            context.PSSetShaderResources(0, Some(&[None]));
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
        let resized = make_image(&[[0.; 3], [25.; 3], [0.; 3], [25.; 3]], 1, 2);
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
