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

/// The precompiled bytecode for `entry` and `target`, or the HLSL compiled now.
///
/// # Safety
///
/// `entry` and `target` must be NUL-terminated.
unsafe fn compile(entry: &'static [u8], target: &'static [u8]) -> Result<Cow<'static, [u8]>> {
    if let Some(bytecode) = shader_bytecode(entry, target) {
        return Ok(Cow::Borrowed(bytecode));
    }
    // SAFETY: `SHADER` is passed with its exact length, callers pass NUL-terminated `entry` and
    // `target`, and each blob is read only within GetBufferSize bytes while it is still alive.
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
        // An HDR desktop hands an SDR stream FP16 frames, which the shader
        // takes back to sRGB; only PQ has no SDR path. Refusing FP16 failed
        // every fallback from the compute queue on such a desktop.
        if !config.hdr && source.2 == Pixel::Rgba10Pq {
            bail!("HDR surface supplied to an SDR converter");
        }
        // SAFETY: `gpu.device` is a live D3D11 device, `compile` gets NUL-terminated names, and
        // every descriptor and the 80-byte `values` outlive the calls that read them.
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
        // SAFETY: `gpu.device` is live, each descriptor outlives its call, and the views target the
        // new texture.
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
        // SAFETY: `image.texture` is live and on this device, which callers check with
        // `accepts_gpu_device`.
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
        // SAFETY: The context is held from Enter to Leave with no early return between, all bound
        // objects are on this device, and `values` is the buffer's 80 bytes.
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
        // SAFETY: `gpu.device` is a live D3D11 device, `compile` gets NUL-terminated names, and
        // every descriptor outlives the call that reads it.
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
                        // Unordered access for the conversion on the compute queue.
                        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0
                            | D3D11_BIND_RENDER_TARGET.0
                            | D3D11_BIND_UNORDERED_ACCESS.0)
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
        // SAFETY: The context is held from Enter to Leave with no early return between, all bound
        // objects are on this device, and `values` is the buffer's 80 bytes.
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
mod tests;
