//! Desktop Duplication's separately supplied hardware pointer.
use crate::capture::Device;
use anyhow::{Result, bail};
use windows::Win32::Graphics::{
    Direct3D11::*,
    Dxgi::{Common::*, *},
};

struct Shape {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    logic: bool,
}
fn decode(info: &DXGI_OUTDUPL_POINTER_SHAPE_INFO, bytes: &[u8]) -> Result<Shape> {
    let mono = info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32;
    let masked = info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 as u32;
    if !mono && !masked && info.Type != DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 as u32 {
        bail!("unsupported desktop pointer shape");
    }
    let height = if mono { info.Height / 2 } else { info.Height };
    let width = info.Width;
    let row = if mono {
        width.div_ceil(8)
    } else {
        width.saturating_mul(4)
    };
    if width == 0
        || height == 0
        || width > 1024
        || height > 1024
        || info.Pitch < row
        || (mono && !info.Height.is_multiple_of(2))
        || u64::from(info.Pitch) * u64::from(info.Height) > bytes.len() as u64
    {
        bail!("invalid desktop pointer dimensions or pitch");
    }
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height as usize {
        for x in 0..width as usize {
            if mono {
                let offset = y * info.Pitch as usize + x / 8;
                let bit = 0x80 >> (x % 8);
                let and = bytes[offset] & bit != 0;
                let xor = bytes[offset + height as usize * info.Pitch as usize] & bit != 0;
                let value = if xor { 255 } else { 0 };
                // Logic alpha: 0 transparent, 128 XOR, 255 replacement.
                pixels.extend([
                    value,
                    value,
                    value,
                    if and { if xor { 128 } else { 0 } } else { 255 },
                ]);
            } else {
                let offset = y * info.Pitch as usize + x * 4;
                let p = &bytes[offset..offset + 4];
                pixels.extend([
                    p[2],
                    p[1],
                    p[0],
                    if masked {
                        if p[3] == 0 { 255 } else { 128 }
                    } else {
                        p[3]
                    },
                ]);
            }
        }
    }
    Ok(Shape {
        width,
        height,
        pixels,
        logic: mono || masked,
    })
}
#[derive(Clone)]
pub(crate) struct Cursor {
    pub view: ID3D11ShaderResourceView,
    pub width: u32,
    pub height: u32,
    pub position: [i32; 2],
    pub logic: bool,
    pub(crate) pixels: std::sync::Arc<[u8]>,
}
impl Cursor {
    pub fn new(gpu: &Device, info: &DXGI_OUTDUPL_POINTER_SHAPE_INFO, bytes: &[u8]) -> Result<Self> {
        let shape = decode(info, bytes)?;
        Self::upload(
            gpu,
            shape.width,
            shape.height,
            shape.logic,
            shape.pixels.into(),
        )
    }
    /// A decoded shape as a texture on `gpu`.
    fn upload(
        gpu: &Device,
        width: u32,
        height: u32,
        logic: bool,
        pixels: std::sync::Arc<[u8]>,
    ) -> Result<Self> {
        let mut texture = None;
        let mut view = None;
        unsafe {
            gpu.device.CreateTexture2D(
                &D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_IMMUTABLE,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    ..Default::default()
                },
                Some(&D3D11_SUBRESOURCE_DATA {
                    pSysMem: pixels.as_ptr().cast(),
                    SysMemPitch: width * 4,
                    SysMemSlicePitch: 0,
                }),
                Some(&mut texture),
            )?;
            gpu.device.CreateShaderResourceView(
                texture.as_ref().unwrap(),
                None,
                Some(&mut view),
            )?;
        }
        Ok(Self {
            view: view.unwrap(),
            width,
            height,
            position: [0; 2],
            logic,
            pixels,
        })
    }
    pub fn blend(&self, image: &mut crate::capture::Image) {
        use crate::capture::Pixel;
        let step = if image.pixel == Pixel::RgbaF16 { 8 } else { 4 };
        for y in 0..self.height as i32 {
            for x in 0..self.width as i32 {
                let (at_x, at_y) = (x + self.position[0], y + self.position[1]);
                if at_x < 0 || at_y < 0 || at_x >= image.width as i32 || at_y >= image.height as i32
                {
                    continue;
                }
                let c = &self.pixels[((y as u32 * self.width + x as u32) * 4) as usize..][..4];
                if c[3] == 0 {
                    continue;
                }
                let at = at_y as usize * image.stride + at_x as usize * step;
                let bytes = &mut image.bytes[at..at + step];
                if image.pixel == Pixel::Bgra8 {
                    for (dst, src) in bytes[..3].iter_mut().zip([c[2], c[1], c[0]]) {
                        *dst = if self.logic && c[3] == 128 {
                            *dst ^ src
                        } else {
                            ((u32::from(*dst) * (255 - u32::from(c[3]))
                                + u32::from(src) * u32::from(c[3])
                                + 127)
                                / 255) as u8
                        };
                    }
                } else {
                    let mut rgb = crate::color::pointer_read(bytes, image.pixel);
                    let alpha = f32::from(c[3]) / 255.;
                    for channel in 0..3 {
                        if self.logic && c[3] == 128 {
                            rgb[channel] = crate::color::pointer_linear(
                                (crate::color::pointer_srgb(rgb[channel]) * 255.).round() as u8
                                    ^ c[channel],
                            );
                        } else {
                            rgb[channel] = rgb[channel] * (1. - alpha)
                                + crate::color::pointer_linear(c[channel]) * alpha;
                        }
                    }
                    crate::color::pointer_write(bytes, image.pixel, rgb);
                }
            }
        }
    }
}
#[derive(Default)]
pub(crate) struct State {
    cursor: Option<Cursor>,
    position: [i32; 2],
    visible: bool,
}
/// The pointer as last reported, carried into a capture made after a
/// restart. Desktop Duplication reports a shape only when it changes, so a
/// new duplication would draw no pointer until the shape next changed. Holds
/// no GPU object: a restart replaces the device, and Windows keeps handing
/// out the old adapter while anything still holds the old device.
#[derive(Clone, Default)]
pub(crate) struct Carried {
    shape: Option<(u32, u32, bool, std::sync::Arc<[u8]>)>,
    position: [i32; 2],
    visible: bool,
}
impl State {
    pub fn carry(&self) -> Carried {
        Carried {
            shape: self
                .cursor
                .as_ref()
                .map(|c| (c.width, c.height, c.logic, c.pixels.clone())),
            position: self.position,
            visible: self.visible,
        }
    }
    /// Start from `carried` until Desktop Duplication reports otherwise.
    pub fn resume(&mut self, gpu: &Device, carried: Carried) -> Result<()> {
        if self.cursor.is_some() {
            return Ok(());
        }
        if let Some((width, height, logic, pixels)) = carried.shape {
            self.cursor = Some(Cursor::upload(gpu, width, height, logic, pixels)?);
            self.position = carried.position;
            self.visible = carried.visible;
        }
        Ok(())
    }
    pub fn update(
        &mut self,
        gpu: &Device,
        duplicate: &IDXGIOutputDuplication,
        info: &DXGI_OUTDUPL_FRAME_INFO,
    ) -> Result<()> {
        if info.PointerShapeBufferSize > 0 {
            if info.PointerShapeBufferSize > 8 * 1024 * 1024 {
                bail!("desktop pointer buffer is too large");
            }
            let mut bytes = vec![0; info.PointerShapeBufferSize as usize];
            let mut shape = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
            let mut required = 0;
            unsafe {
                duplicate.GetFramePointerShape(
                    bytes.len() as u32,
                    bytes.as_mut_ptr().cast(),
                    &mut required,
                    &mut shape,
                )?;
            }
            if required as usize > bytes.len() {
                bail!("desktop pointer buffer changed size");
            }
            self.cursor = Some(Cursor::new(gpu, &shape, &bytes)?);
        }
        if info.LastMouseUpdateTime != 0 {
            self.position = [
                info.PointerPosition.Position.x,
                info.PointerPosition.Position.y,
            ];
            self.visible = info.PointerPosition.Visible.as_bool();
        }
        Ok(())
    }
    /// Whether the pointer is on screen: a shape and visible.
    pub fn shown(&self) -> bool {
        self.cursor.is_some() && self.visible
    }
    pub fn snapshot(&self) -> Option<Cursor> {
        self.cursor
            .as_ref()
            .filter(|_| self.visible)
            .map(|cursor| Cursor {
                position: self.position,
                ..cursor.clone()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    #[test]
    #[ignore = "requires a D3D11 device"]
    fn a_restarted_capture_draws_the_last_pointer_until_duplication_reports_one() -> Result<()> {
        let _com = crate::capture::ComGuard::new()?;
        let info = DXGI_OUTDUPL_POINTER_SHAPE_INFO {
            Type: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 as u32,
            Width: 2,
            Height: 2,
            Pitch: 8,
            ..Default::default()
        };
        let bytes: Vec<u8> = (0..16).collect();
        let carried = {
            let first = Device::new("")?;
            let mut state = State {
                cursor: Some(Cursor::new(&first, &info, &bytes)?),
                position: [10, 20],
                visible: true,
            };
            let carried = state.carry();
            state.visible = false;
            assert!(state.carry().shape.is_some() && !state.carry().visible);
            carried
        };
        // A new device, as a capture restart makes.
        let second = Device::new("")?;
        let mut state = State::default();
        assert!(state.snapshot().is_none());
        state.resume(&second, carried.clone())?;
        let cursor = state
            .snapshot()
            .context("the carried pointer is not drawn")?;
        assert_eq!(
            (cursor.position, cursor.width, cursor.height),
            ([10, 20], 2, 2)
        );
        assert_eq!(&cursor.pixels[..4], &[2, 1, 0, 3]);
        // A pointer the new duplication reported already is kept.
        let mut reported = State {
            cursor: Some(Cursor::new(&second, &info, &bytes)?),
            position: [1, 1],
            visible: true,
        };
        reported.resume(&second, carried)?;
        assert_eq!(reported.snapshot().unwrap().position, [1, 1]);
        // A hidden pointer stays hidden.
        let mut hidden = State::default();
        hidden.resume(
            &second,
            Carried {
                visible: false,
                ..state.carry()
            },
        )?;
        assert!(hidden.snapshot().is_none());
        Ok(())
    }
    #[test]
    fn monochrome_pointer_preserves_transparency_replacement_and_inversion() -> Result<()> {
        let info = DXGI_OUTDUPL_POINTER_SHAPE_INFO {
            Type: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32,
            Width: 4,
            Height: 2,
            Pitch: 1,
            ..Default::default()
        };
        let shape = decode(&info, &[0b00110000, 0b01010000])?;
        assert!(shape.logic);
        assert_eq!(shape.height, 1);
        assert_eq!(
            shape.pixels,
            [
                0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 0, 255, 255, 255, 128
            ]
        );
        assert!(decode(&info, &[0]).is_err());
        Ok(())
    }
    #[test]
    fn color_and_masked_shapes_keep_row_pitch_and_bgra_order() -> Result<()> {
        let mut info = DXGI_OUTDUPL_POINTER_SHAPE_INFO {
            Type: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 as u32,
            Width: 1,
            Height: 2,
            Pitch: 8,
            ..Default::default()
        };
        let data = [1, 2, 3, 255, 0, 0, 0, 0, 4, 5, 6, 0, 0, 0, 0, 0];
        assert_eq!(decode(&info, &data)?.pixels, [3, 2, 1, 255, 6, 5, 4, 0]);
        info.Type = DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 as u32;
        assert_eq!(decode(&info, &data)?.pixels, [3, 2, 1, 128, 6, 5, 4, 255]);
        info.Pitch = 3;
        assert!(decode(&info, &data).is_err());
        Ok(())
    }
}
