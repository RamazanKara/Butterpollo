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
    uint pixel; uint hdr; uint2 padding;
};
float4 vertex(uint id : SV_VertexID) : SV_Position {
    float2 p = float2((id << 1) & 2, id & 2);
    return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
}
float3 load(int2 p) {
    float3 rgb = source.Load(int3(clamp(p, int2(0, 0), int2(sourceSize) - 1), 0)).rgb;
    if (hdr != 0 && pixel == 0) {
        rgb = lerp(pow((rgb + 0.055) / 1.055, 2.4), rgb / 12.92, step(rgb, 0.04045));
    }
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
float3 weights() { return hdr != 0 ? float3(0.2627, 0.6780, 0.0593) : float3(0.2126, 0.7152, 0.0722); }
float4 luma(float4 p : SV_Position) : SV_Target {
    float y = dot(nonlinear(p.xy - 0.5), weights());
    float code = hdr != 0 ? floor(clamp(64 + 876 * y, 0, 1023) + 0.5) * 64 / 65535.0
                         : floor(clamp(16 + 219 * y, 0, 255) + 0.5) / 255.0;
    return float4(code, 0, 0, 1);
}
float4 chroma(float4 p : SV_Position) : SV_Target {
    float2 at = floor(p.xy) * 2;
    float3 rgb = (nonlinear(at) + nonlinear(at + float2(1, 0)) +
                  nonlinear(at + float2(0, 1)) + nonlinear(at + float2(1, 1))) * 0.25;
    float3 k = weights();
    float y = dot(rgb, k);
    float2 uv = float2((rgb.b - y) / (2 * (1 - k.b)), (rgb.r - y) / (2 * (1 - k.r)));
    float2 code = hdr != 0 ? floor(clamp(512 + 896 * uv, 0, 1023) + 0.5) * 64 / 65535.0
                          : floor(clamp(128 + 224 * uv, 0, 255) + 0.5) / 255.0;
    return float4(code, 0, 1);
}
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
    chroma: ID3D11RenderTargetView,
}
pub(crate) struct Converter {
    gpu: Device,
    vertex: ID3D11VertexShader,
    luma: ID3D11PixelShader,
    chroma: ID3D11PixelShader,
    constants: ID3D11Buffer,
    width: u32,
    height: u32,
    hdr: bool,
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
            || !config.width.is_multiple_of(2)
            || !config.height.is_multiple_of(2)
        {
            bail!("GPU 4:2:0 conversion requires even, nonzero dimensions");
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
            gpu.device.CreatePixelShader(
                &compile(b"luma\0", b"ps_5_0\0")?,
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
                0,
                0,
            ];
            let mut constants = None;
            gpu.device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 32,
                    Usage: D3D11_USAGE_IMMUTABLE,
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
                width: config.width,
                height: config.height,
                hdr: config.hdr,
                targets: vec![],
            })
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
                        Height: self.height,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: if self.hdr {
                            DXGI_FORMAT_P010
                        } else {
                            DXGI_FORMAT_NV12
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
            let luma = view(if self.hdr {
                DXGI_FORMAT_R16_UNORM
            } else {
                DXGI_FORMAT_R8_UNORM
            })?;
            let chroma = view(if self.hdr {
                DXGI_FORMAT_R16G16_UNORM
            } else {
                DXGI_FORMAT_R8G8_UNORM
            })?;
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
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(&self.vertex, None);
            context.PSSetShaderResources(0, Some(&[source]));
            context.PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            context.RSSetState(None);
            context.OMSetBlendState(None, None, u32::MAX);
            for (view, shader, divisor) in [
                (&self.targets[index].luma, &self.luma, 1),
                (&self.targets[index].chroma, &self.chroma, 2),
            ] {
                context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                    Width: (self.width / divisor) as f32,
                    Height: (self.height / divisor) as f32,
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
            for y in 0..desc.Height as usize * 3 / 2 {
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
}
