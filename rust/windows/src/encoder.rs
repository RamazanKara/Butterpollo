//! Video encoder selection and the FFmpeg-backed encoders.
//!
//! [`Encoder`] picks the native AMF or NVENC encoder for the capture GPU and
//! falls back to FFmpeg's NVENC, Quick Sync or software encoders when the
//! native one fails. [`Ffmpeg`] wraps libavcodec, and [`Convert`] turns
//! captured frames into the pixel format an FFmpeg encoder takes.
mod gpu;

use crate::{
    capture::{GpuImage, Image},
    ff,
};
use anyhow::{Context, Result, bail};
use butterpollo_core::{rtsp::Negotiated, session::Warnings};
use std::{
    ffi::{CStr, CString},
    ptr,
};

pub struct Encoded {
    pub bytes: Vec<u8>,
    pub idr: bool,
    pub after_invalidation: bool,
    /// Submission through completed codec output, including asynchronous work.
    pub latency: Option<std::time::Duration>,
    pub presentation: Option<std::time::Instant>,
}
pub(crate) fn check(code: i32) -> Result<()> {
    if code >= 0 {
        return Ok(());
    }
    let mut text = [0i8; 256];
    // SAFETY: text is writable for the exact capacity supplied to av_strerror.
    unsafe {
        ff::av_strerror(code, text.as_mut_ptr(), text.len());
    }
    bail!(
        "FFmpeg: {}",
        // SAFETY: av_strerror writes a terminated string within text, which was
        // zero-initialized and remains live while CStr borrows it.
        unsafe { CStr::from_ptr(text.as_ptr()) }.to_string_lossy()
    )
}
pub(crate) fn c(s: &str) -> CString {
    CString::new(s).expect("internal codec string contains NUL")
}
/// # Safety
/// Non-null `data` must hold `size` initialized bytes, with a nonnegative size
/// no greater than `isize::MAX`, valid and unmodified for the packet's borrow.
#[inline]
unsafe fn packet_bytes(packet: &ff::AVPacket) -> &[u8] {
    if packet.size == 0 || packet.data.is_null() {
        &[]
    } else {
        // SAFETY: The caller guarantees a readable, initialized buffer for the
        // packet's lifetime; null pointers and empty payloads are handled above.
        unsafe { std::slice::from_raw_parts(packet.data, packet.size as usize) }
    }
}
pub struct Convert {
    pub(crate) frame: *mut ff::AVFrame,
    context: *mut ff::SwsContext,
    width: u32,
    height: u32,
    pixel: i32,
    source: (u32, u32),
    /// The picture's size inside the frame.
    content: (u32, u32),
    hdr: bool,
    matrix: u8,
    full_range: bool,
    scratch: Vec<u8>,
    pub(crate) luminance: [f32; 2],
}
impl Convert {
    pub fn new(width: u32, height: u32, pixel: i32) -> Result<Self> {
        // SAFETY: The allocated frame is checked for null before access and transferred
        // to Self for one av_frame_free; av_frame_get_buffer validates the format and
        // dimensions and allocates the planes before a usable converter is returned.
        unsafe {
            let frame = ff::av_frame_alloc();
            if frame.is_null() {
                bail!("cannot allocate frame");
            }
            (*frame).format = pixel;
            (*frame).width = width as i32;
            (*frame).height = height as i32;
            let s = Self {
                frame,
                context: ptr::null_mut(),
                width,
                height,
                pixel,
                source: (0, 0),
                content: (0, 0),
                hdr: matches!(
                    pixel,
                    ff::AVPixelFormat_AV_PIX_FMT_P010LE
                        | ff::AVPixelFormat_AV_PIX_FMT_YUV420P10LE
                        | ff::AVPixelFormat_AV_PIX_FMT_YUV444P10LE
                        | ff::AVPixelFormat_AV_PIX_FMT_YUV444P16LE
                ),
                scratch: vec![],
                luminance: [100., 1.],
                matrix: if matches!(
                    pixel,
                    ff::AVPixelFormat_AV_PIX_FMT_P010LE
                        | ff::AVPixelFormat_AV_PIX_FMT_YUV420P10LE
                        | ff::AVPixelFormat_AV_PIX_FMT_YUV444P10LE
                        | ff::AVPixelFormat_AV_PIX_FMT_YUV444P16LE
                ) {
                    2
                } else {
                    1
                },
                full_range: false,
            };
            check(ff::av_frame_get_buffer(s.frame, 32))?;
            Ok(s)
        }
    }
    pub fn new_config(config: &Negotiated, pixel: i32) -> Result<Self> {
        let mut converter = Self::new(config.width, config.height, pixel)?;
        converter.hdr = config.hdr;
        converter.matrix = config.color_matrix();
        converter.full_range = config.full_range();
        Ok(converter)
    }
    pub fn convert(&mut self, image: &Image) -> Result<()> {
        let pixel_bytes = if image.pixel == crate::capture::Pixel::RgbaF16 {
            8
        } else {
            4
        };
        if image.width == 0
            || image.height == 0
            || image.stride < image.width as usize * pixel_bytes
            || image.bytes.len() < image.stride * image.height as usize
        {
            bail!("invalid captured image layout");
        }
        // A source of another shape keeps its aspect ratio between black
        // bars, as in the GPU converter.
        let (x, y, content_width, content_height) = butterpollo_core::display_policy::letterbox(
            (image.width, image.height),
            (self.width, self.height),
        );
        if self.hdr {
            crate::color::hdr_rgba_scaled_luminance(
                image,
                content_width,
                content_height,
                self.luminance,
                &mut self.scratch,
            );
        } else {
            if image.pixel != crate::capture::Pixel::Bgra8 {
                bail!("HDR surface supplied to an SDR converter");
            }
            self.scratch.clear();
            self.scratch
                .extend_from_slice(&image.bytes[..image.stride * image.height as usize]);
        }
        // AVFrame documents swscale reads up to 16 bytes past a plane; FFmpeg's
        // input-buffer padding covers those reads for both RGBA64 and BGRA.
        self.scratch.resize(
            self.scratch.len() + ff::AV_INPUT_BUFFER_PADDING_SIZE as usize,
            0,
        );
        // SAFETY: The source has a validated nonzero layout and initialized tail
        // padding. The destination is owned by this converter and was allocated
        // by av_frame_get_buffer with aligned, padded planes.
        unsafe {
            check(ff::av_frame_make_writable(self.frame))?;
            let source = if self.hdr {
                (content_width, content_height)
            } else {
                (image.width, image.height)
            };
            if self.context.is_null()
                || self.source != source
                || self.content != (content_width, content_height)
            {
                if !self.context.is_null() {
                    ff::sws_freeContext(self.context);
                }
                self.context = ff::sws_getContext(
                    source.0 as i32,
                    source.1 as i32,
                    if self.hdr {
                        ff::AVPixelFormat_AV_PIX_FMT_RGBA64LE
                    } else {
                        ff::AVPixelFormat_AV_PIX_FMT_BGRA
                    },
                    content_width as i32,
                    content_height as i32,
                    self.pixel,
                    1,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null(),
                );
                if self.context.is_null() {
                    bail!("cannot initialize pixel conversion");
                }
                let matrix = ff::sws_getCoefficients(match self.matrix {
                    0 => 5,
                    2 => 9,
                    _ => 1,
                });
                check(ff::sws_setColorspaceDetails(
                    self.context,
                    matrix,
                    1,
                    matrix,
                    i32::from(self.full_range),
                    0,
                    65536,
                    65536,
                ))?;
                self.source = source;
                self.content = (content_width, content_height);
            }
            let src = [self.scratch.as_ptr(), ptr::null(), ptr::null(), ptr::null()];
            let strides = [
                if self.hdr {
                    content_width as i32 * 8
                } else {
                    image.stride as i32
                },
                0,
                0,
                0,
            ];
            (*self.frame).color_primaries = if self.hdr {
                ff::AVColorPrimaries_AVCOL_PRI_BT2020
            } else if self.matrix == 0 {
                ff::AVColorPrimaries_AVCOL_PRI_SMPTE170M
            } else if self.matrix == 2 {
                ff::AVColorPrimaries_AVCOL_PRI_BT2020
            } else {
                ff::AVColorPrimaries_AVCOL_PRI_BT709
            };
            (*self.frame).color_trc = if self.hdr {
                ff::AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084
            } else if self.matrix == 0 {
                ff::AVColorTransferCharacteristic_AVCOL_TRC_SMPTE170M
            } else if self.matrix == 2 {
                ff::AVColorTransferCharacteristic_AVCOL_TRC_BT2020_10
            } else {
                ff::AVColorTransferCharacteristic_AVCOL_TRC_BT709
            };
            (*self.frame).colorspace = if self.matrix == 2 {
                ff::AVColorSpace_AVCOL_SPC_BT2020_NCL
            } else if self.matrix == 0 {
                ff::AVColorSpace_AVCOL_SPC_SMPTE170M
            } else {
                ff::AVColorSpace_AVCOL_SPC_BT709
            };
            (*self.frame).color_range = if self.full_range {
                ff::AVColorRange_AVCOL_RANGE_JPEG
            } else {
                ff::AVColorRange_AVCOL_RANGE_MPEG
            };
            let mut destination = (*self.frame).data;
            if (content_width, content_height) != (self.width, self.height) {
                // The bars, then the picture into the rectangle between them.
                let linesize: [isize; 4] =
                    std::array::from_fn(|plane| (*self.frame).linesize[plane] as isize);
                check(ff::av_image_fill_black(
                    (*self.frame).data.as_ptr(),
                    linesize.as_ptr(),
                    self.pixel,
                    (*self.frame).color_range,
                    self.width as i32,
                    self.height as i32,
                ))?;
                for (plane, offset) in plane_offsets(self.frame, self.pixel, x, y)?
                    .into_iter()
                    .enumerate()
                {
                    if !destination[plane].is_null() {
                        destination[plane] = destination[plane].add(offset);
                    }
                }
            }
            check(ff::sws_scale(
                self.context,
                src.as_ptr(),
                strides.as_ptr(),
                0,
                self.source.1 as i32,
                destination.as_ptr(),
                (*self.frame).linesize.as_ptr(),
            ))?;
            Ok(())
        }
    }
}
/// Byte offsets of pixel (x, y) in each plane of `frame`.
unsafe fn plane_offsets(
    frame: *const ff::AVFrame,
    pixel: i32,
    x: u32,
    y: u32,
) -> Result<[usize; 4]> {
    // SAFETY: Callers retain a live AVFrame with linesizes for pixel; FFmpeg returns
    // a static descriptor or null, checked before access, with at most four components.
    unsafe {
        let Some(format) = ff::av_pix_fmt_desc_get(pixel).as_ref() else {
            bail!("unknown pixel format {pixel}");
        };
        let components = &format.comp[..usize::from(format.nb_components)];
        let mut offsets = [0; 4];
        for (plane, offset) in offsets.iter_mut().enumerate() {
            let Some(step) = components
                .iter()
                .filter(|c| c.plane as usize == plane)
                .map(|c| c.step as usize)
                .max()
            else {
                continue;
            };
            // Planes 1 and 2 hold subsampled chroma.
            let (shift_x, shift_y) = if matches!(plane, 1 | 2) {
                (format.log2_chroma_w, format.log2_chroma_h)
            } else {
                (0, 0)
            };
            *offset = (y as usize >> shift_y) * (*frame).linesize[plane] as usize
                + (x as usize >> shift_x) * step;
        }
        Ok(offsets)
    }
}
impl Drop for Convert {
    fn drop(&mut self) {
        // SAFETY: The frame and non-null context are exclusively owned allocations
        // from FFmpeg, and no conversion is running when they are freed once here.
        unsafe {
            ff::av_frame_free(&mut self.frame);
            if !self.context.is_null() {
                ff::sws_freeContext(self.context);
            }
        }
    }
}
pub struct Ffmpeg {
    context: *mut ff::AVCodecContext,
    packet: *mut ff::AVPacket,
    convert: Option<Convert>,
    native: Option<gpu::Native>,
    config: Negotiated,
    software_pixel: i32,
    luminance: [f32; 2],
    index: i64,
    pub name: String,
    presentations: std::collections::BTreeMap<i64, std::time::Instant>,
    staging: Option<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D>,
}
impl Ffmpeg {
    pub fn new(config: &Negotiated, name: &str) -> Result<Self> {
        Self::new_options(config, name, &butterpollo_core::config::Config::default())
    }
    pub fn new_options(
        config: &Negotiated,
        name: &str,
        tuning: &butterpollo_core::config::Config,
    ) -> Result<Self> {
        Self::open(config, name, tuning, None)
    }
    fn new_gpu_options(
        config: &Negotiated,
        name: &str,
        tuning: &butterpollo_core::config::Config,
        image: &GpuImage,
    ) -> Result<Self> {
        Self::open(config, name, tuning, Some(image))
    }
    fn open(
        config: &Negotiated,
        name: &str,
        tuning: &butterpollo_core::config::Config,
        image: Option<&GpuImage>,
    ) -> Result<Self> {
        let settings = butterpollo_core::encoder_policy::ffmpeg(tuning, config, name)?;
        // SAFETY: FFmpeg's codec descriptor is static, allocations are null-checked
        // before access, and context/packet ownership is transferred to Self or freed
        // on error. CString arguments live through calls; dictionary entries are copied
        // before freeing the dictionary, and native.frames() transfers an owned reference.
        unsafe {
            let codec = ff::avcodec_find_encoder_by_name(c(name).as_ptr());
            if codec.is_null() {
                bail!("encoder {name} is unavailable in this SDK");
            }
            let hardware = name.ends_with("_nvenc") || name.ends_with("_qsv");
            if config.ten_bit() && config.codec == 0 {
                bail!("H.264 cannot carry the negotiated HDR10 stream");
            }
            if name.ends_with("_qsv") && config.yuv444 {
                bail!("QSV 4:4:4 is unavailable in this SDK");
            }
            let native = image
                .map(|image| gpu::Native::new(image, config, name.ends_with("_qsv")))
                .transpose()?;
            let context = ff::avcodec_alloc_context3(codec);
            if context.is_null() {
                bail!("cannot allocate encoder");
            }
            let pixel = if hardware {
                if config.yuv444 {
                    if config.ten_bit() {
                        ff::AVPixelFormat_AV_PIX_FMT_YUV444P16LE
                    } else {
                        ff::AVPixelFormat_AV_PIX_FMT_YUV444P
                    }
                } else if config.ten_bit() {
                    ff::AVPixelFormat_AV_PIX_FMT_P010LE
                } else {
                    ff::AVPixelFormat_AV_PIX_FMT_NV12
                }
            } else if config.ten_bit() {
                if config.yuv444 {
                    ff::AVPixelFormat_AV_PIX_FMT_YUV444P10LE
                } else {
                    ff::AVPixelFormat_AV_PIX_FMT_YUV420P10LE
                }
            } else if config.yuv444 {
                ff::AVPixelFormat_AV_PIX_FMT_YUV444P
            } else {
                ff::AVPixelFormat_AV_PIX_FMT_YUV420P
            };
            let init = {
                (*context).width = config.width as i32;
                (*context).height = config.height as i32;
                (*context).pix_fmt = native.as_ref().map_or(pixel, |native| native.pixel());
                (*context).color_primaries = if config.hdr {
                    ff::AVColorPrimaries_AVCOL_PRI_BT2020
                } else if config.color_matrix() == 0 {
                    ff::AVColorPrimaries_AVCOL_PRI_SMPTE170M
                } else if config.color_matrix() == 2 {
                    ff::AVColorPrimaries_AVCOL_PRI_BT2020
                } else {
                    ff::AVColorPrimaries_AVCOL_PRI_BT709
                };
                (*context).color_trc = if config.hdr {
                    ff::AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084
                } else if config.color_matrix() == 0 {
                    ff::AVColorTransferCharacteristic_AVCOL_TRC_SMPTE170M
                } else if config.color_matrix() == 2 {
                    ff::AVColorTransferCharacteristic_AVCOL_TRC_BT2020_10
                } else {
                    ff::AVColorTransferCharacteristic_AVCOL_TRC_BT709
                };
                (*context).colorspace = if config.color_matrix() == 2 {
                    ff::AVColorSpace_AVCOL_SPC_BT2020_NCL
                } else if config.color_matrix() == 0 {
                    ff::AVColorSpace_AVCOL_SPC_SMPTE170M
                } else {
                    ff::AVColorSpace_AVCOL_SPC_BT709
                };
                (*context).color_range = if config.full_range() {
                    ff::AVColorRange_AVCOL_RANGE_JPEG
                } else {
                    ff::AVColorRange_AVCOL_RANGE_MPEG
                };
                (*context).time_base = ff::AVRational {
                    num: 1000,
                    den: config.fps_millihz() as i32,
                };
                (*context).framerate = ff::AVRational {
                    num: config.fps_millihz() as i32,
                    den: 1000,
                };
                (*context).bit_rate = i64::from(config.bitrate_kbps) * 1000;
                (*context).rc_max_rate = (*context).bit_rate;
                let vbv_increase = if name.ends_with("_nvenc") {
                    tuning.integer("nvenc_vbv_increase", 0).clamp(0, 400)
                } else {
                    0
                };
                (*context).rc_buffer_size =
                    ((*context).bit_rate * 1000 / config.fps_millihz() as i64
                        * (100 + vbv_increase)
                        / 100)
                        .clamp(1000, i32::MAX as i64) as i32;
                (*context).gop_size = i32::MAX;
                (*context).max_b_frames = 0;
                (*context).thread_count = if hardware {
                    1
                } else {
                    tuning.integer("min_threads", 2).clamp(1, 64) as i32
                };
                if config.references > 0 {
                    (*context).refs = config.references as i32;
                }
                if config.slices > 1 {
                    (*context).slices = config.slices as i32;
                }
                (*context).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as i32;
                let mut options = ptr::null_mut();
                let set = |options: &mut *mut ff::AVDictionary, key: &str, value: &str| {
                    ff::av_dict_set(options, c(key).as_ptr(), c(value).as_ptr(), 0);
                };
                for (key, value) in &settings {
                    set(&mut options, key, value);
                }
                let result = (|| -> Result<()> {
                    if let Some(native) = &native {
                        (*context).hw_frames_ctx = native.frames()?;
                    }
                    check(ff::avcodec_open2(context, codec, &mut options))
                })();
                let unused = ff::av_dict_get(
                    options,
                    c("").as_ptr(),
                    ptr::null(),
                    ff::AV_DICT_IGNORE_SUFFIX as i32,
                );
                let unused = if unused.is_null() {
                    None
                } else {
                    Some(CStr::from_ptr((*unused).key).to_string_lossy().into_owned())
                };
                ff::av_dict_free(&mut options);
                result.and_then(|()| {
                    if let Some(key) = unused {
                        bail!("encoder {name} does not support option {key}")
                    } else {
                        Ok(())
                    }
                })
            };
            if let Err(e) = init {
                let mut p = context;
                ff::avcodec_free_context(&mut p);
                return Err(e);
            }
            let packet = ff::av_packet_alloc();
            if packet.is_null() {
                let mut p = context;
                ff::avcodec_free_context(&mut p);
                bail!("cannot allocate encoded packet");
            }
            Ok(Self {
                context,
                packet,
                convert: None,
                native,
                config: config.clone(),
                software_pixel: pixel,
                luminance: [100., 1.],
                index: 0,
                name: name.into(),
                presentations: Default::default(),
                staging: None,
            })
        }
    }
    pub fn encode(&mut self, image: &Image, idr: bool, bitrate_kbps: u32) -> Result<Vec<Encoded>> {
        if let Some(native) = &self.native {
            let image = GpuImage::upload(&native.device, image)?;
            return self.encode_gpu(&image, idr, bitrate_kbps);
        }
        if self.convert.is_none() {
            self.convert = Some(Convert::new_config(&self.config, self.software_pixel)?);
        }
        let convert = self.convert.as_mut().unwrap();
        convert.luminance = self.luminance;
        convert.convert(image)?;
        let frame = convert.frame;
        self.encode_frame(frame, idr, bitrate_kbps, image.captured)
    }
    fn encode_gpu(&mut self, image: &GpuImage, idr: bool, bitrate: u32) -> Result<Vec<Encoded>> {
        if let Some(native) = self.native.as_mut() {
            let frame = native.frame(image)?;
            self.encode_frame(frame, idr, bitrate, image.captured)
        } else {
            let image = image.readback(&mut self.staging)?;
            self.encode(&image, idr, bitrate)
        }
    }
    fn encode_frame(
        &mut self,
        frame: *mut ff::AVFrame,
        idr: bool,
        bitrate_kbps: u32,
        presentation: std::time::Instant,
    ) -> Result<Vec<Encoded>> {
        // SAFETY: Internal callers retain the live frame, while self owns the codec
        // context and packet exclusively. Successful receive initializes the packet,
        // whose bounded data is copied before unref. Slice creation also requires
        // non-null data for an empty packet; the size check below does not establish that.
        unsafe {
            let bitrate = i64::from(bitrate_kbps) * 1000;
            if bitrate != (*self.context).bit_rate {
                let old = (*self.context).bit_rate.max(1);
                (*self.context).rc_buffer_size =
                    (i64::from((*self.context).rc_buffer_size).saturating_mul(bitrate) / old)
                        .clamp(1000, i64::from(i32::MAX)) as i32;
                (*self.context).rc_max_rate = bitrate;
                (*self.context).bit_rate = bitrate;
            }
            (*frame).pts = self.index;
            (*frame).color_primaries = (*self.context).color_primaries;
            (*frame).color_trc = (*self.context).color_trc;
            (*frame).colorspace = (*self.context).colorspace;
            (*frame).color_range = (*self.context).color_range;
            (*frame).pict_type = if idr {
                ff::AVPictureType_AV_PICTURE_TYPE_I
            } else {
                ff::AVPictureType_AV_PICTURE_TYPE_NONE
            };
            self.index += 1;
            check(ff::avcodec_send_frame(self.context, frame))?;
            self.presentations.insert((*frame).pts, presentation);
            if self.presentations.len() > 32 {
                bail!("codec failed to return bounded output");
            }
            let mut output = vec![];
            loop {
                if output.len() >= 64 {
                    bail!("codec did not stop returning output packets");
                }
                let code = ff::avcodec_receive_packet(self.context, self.packet);
                if code == -11 || code == -541478725 {
                    break;
                }
                check(code)?;
                let n = (*self.packet).size;
                if !(0..=64 * 1024 * 1024).contains(&n) {
                    bail!("invalid encoder output size");
                }
                // SAFETY: The received packet owns its payload until av_packet_unref
                // below, and its size has been checked before borrowing the bytes.
                let bytes = packet_bytes(&*self.packet).to_vec();
                output.push(Encoded {
                    bytes,
                    idr: (*self.packet).flags & ff::AV_PKT_FLAG_KEY as i32 != 0,
                    after_invalidation: false,
                    latency: None,
                    presentation: self.presentations.remove(&(*self.packet).pts),
                });
                ff::av_packet_unref(self.packet);
            }
            Ok(output)
        }
    }
}
impl Drop for Ffmpeg {
    fn drop(&mut self) {
        // SAFETY: Self owns both FFmpeg allocations and frees each once after all
        // encode calls; the converter and native frame owners remain alive during cleanup.
        unsafe {
            ff::av_packet_free(&mut self.packet);
            ff::avcodec_free_context(&mut self.context);
        }
    }
}
/// Automatic's own encoder for a GPU: AMF on AMD, NVENC on NVIDIA. By PCI
/// vendor, so AMD adapters not named "Radeon" (FirePro, the Steam Deck's
/// "AMD Custom GPU") still use AMF; the name covers a failed vendor query.
fn native_backend(vendor: u32, adapter: &str) -> Option<&'static str> {
    match vendor {
        0x1002 => Some("amf"),
        0x10de => Some("nvenc"),
        _ if adapter.contains("Radeon") => Some("amf"),
        _ if adapter.to_ascii_lowercase().contains("nvidia") => Some("nvenc"),
        _ => None,
    }
}
fn native_for(device: &crate::capture::Device) -> Option<&'static str> {
    use windows::{Win32::Graphics::Dxgi::IDXGIDevice, core::Interface};
    // SAFETY: The borrowed D3D device and each queried COM interface retain their
    // references through the synchronous adapter query; no raw pointers escape.
    let vendor = unsafe {
        device
            .device
            .cast::<IDXGIDevice>()
            .and_then(|device| device.GetAdapter())
            .and_then(|adapter| adapter.GetDesc())
            .map_or(0, |desc| desc.VendorId)
    };
    native_backend(vendor, &device.display.adapter)
}
fn native_name(backend: &str) -> &'static str {
    if backend == "amf" { "AMF" } else { "NVENC" }
}
/// Automatic keeps a stream on another hardware encoder when the GPU's own
/// one fails, and says so; it never falls back to software.
fn fallback_warning(native: &str, error: &str, active: &str) -> String {
    format!(
        "{} could not start ({error}); Automatic is encoding with {active} instead. Expect different quality, latency and GPU load; check the GPU driver and codec, then reconnect.",
        native_name(native)
    )
}
fn refused(native: Option<(&str, anyhow::Error)>, others: &[String]) -> anyhow::Error {
    let others = if others.is_empty() {
        "no other hardware encoder applies".to_owned()
    } else {
        format!("other hardware encoders failed: {}", others.join("; "))
    };
    let remedy = "Automatic never encodes in software. Check the GPU driver and the selected codec, or explicitly select Software at a lower resolution and frame rate";
    match native {
        Some((backend, error)) => error.context(format!(
            "{} failed and {others}. {remedy}",
            native_name(backend)
        )),
        None => anyhow::anyhow!("unable to initialize encoder: {others}. {remedy}"),
    }
}

fn ffmpeg_candidates(codec: u8, preference: &str) -> Result<Vec<String>> {
    let codec_name = match codec {
        0 => "h264",
        1 => "hevc",
        2 => "av1",
        _ => bail!("unsupported codec"),
    };
    Ok(match preference {
        "nvenc_legacy" => vec![format!("{codec_name}_nvenc")],
        "quicksync" | "qsv" => vec![format!("{codec_name}_qsv")],
        "software" => vec![["libx264", "libx265", "libsvtav1"][codec as usize].into()],
        _ => vec![format!("{codec_name}_nvenc"), format!("{codec_name}_qsv")],
    })
}

pub enum Encoder {
    Ffmpeg(Box<Ffmpeg>),
    Amf(Box<crate::amf::Encoder>),
    Nvenc(Box<crate::nvenc::Encoder>),
    Pyrowave(Box<crate::pyrowave::Encoder>),
}
impl Encoder {
    pub fn set_hdr_metadata(&mut self, metadata: butterpollo_core::hdr::Metadata) {
        match self {
            Self::Nvenc(encoder) => encoder.set_hdr_metadata(metadata),
            Self::Amf(encoder) => encoder.set_hdr_metadata(metadata),
            Self::Ffmpeg(_) | Self::Pyrowave(_) => {}
        }
    }
    /// SDR white is absolute luminance; scRGB scaling expands NGX's 1000-nit ceiling.
    pub fn set_luminance(&mut self, white_nits: f32, linear_scale: f32) {
        let luminance = [white_nits.clamp(100., 200.), linear_scale.clamp(1., 2.)];
        match self {
            Self::Amf(e) => e.luminance = luminance,
            Self::Nvenc(e) => e.set_luminance(luminance),
            Self::Ffmpeg(e) => {
                e.luminance = luminance;
                if let Some(native) = e.native.as_mut() {
                    native.set_luminance(luminance);
                }
            }
            Self::Pyrowave(e) => e.luminance = luminance,
        }
    }
    /// The next picture repeats the one last encoded. PyroWave sizes a new
    /// picture from the previous new one, so a repeat just before it does not
    /// shrink it to the few milliseconds since that repeat.
    pub fn set_repeat(&mut self, repeat: bool) {
        if let Self::Pyrowave(e) = self {
            e.repeat = repeat;
        }
    }
    pub fn new_gpu(config: &Negotiated, preference: &str, image: &GpuImage) -> Result<Self> {
        Self::new_gpu_options(
            config,
            preference,
            image,
            &butterpollo_core::config::Config::default(),
        )
    }
    pub fn new_gpu_options(
        config: &Negotiated,
        preference: &str,
        image: &GpuImage,
        tuning: &butterpollo_core::config::Config,
    ) -> Result<Self> {
        Self::new_gpu_reported(config, preference, image, tuning, &Default::default())
    }
    pub fn new_gpu_reported(
        config: &Negotiated,
        preference: &str,
        image: &GpuImage,
        tuning: &butterpollo_core::config::Config,
        warnings: &std::sync::Arc<Warnings>,
    ) -> Result<Self> {
        warnings.clear("encoder_conversion");
        warnings.clear("encoder_readback");
        let preference = butterpollo_core::encoder_policy::canonical_name(preference);
        if config.codec == 3 {
            return Ok(Self::Pyrowave(Box::new(
                crate::pyrowave::Encoder::new_device_reported(
                    config,
                    image.gpu.clone(),
                    tuning,
                    warnings.clone(),
                )?,
            )));
        }
        let automatic = matches!(preference, "" | "auto");
        let native = automatic.then(|| native_for(&image.gpu)).flatten();
        let mut native_error = None;
        if matches!(preference, "nvenc" | "nvenc_experimental") || native == Some("nvenc") {
            match crate::nvenc::Encoder::new_device_options(config, image.gpu.clone(), tuning) {
                Ok(encoder) => {
                    warnings.clear("encoder_fallback");
                    return Ok(Self::Nvenc(Box::new(encoder)));
                }
                Err(error) if !automatic => return Err(error),
                Err(error) => native_error = Some(("nvenc", error)),
            }
        }
        if preference == "amf" || native == Some("amf") {
            // Colour conversion on a compute queue keeps running beside a
            // game that fills the GPU's graphics queue.
            // AMF rejects 4:4:4 regardless of which queue converts the input.
            let compute = if !config.yuv444
                && crate::compute::enabled(tuning)
                && crate::compute::shareable(&image.texture)
            {
                match crate::compute::Compute::for_device(&image.gpu.device) {
                    Ok(compute) => Some(compute),
                    Err(error) => {
                        warnings.set("encoder_conversion", format!("AMF compute conversion unavailable ({error:#}); converting on the graphics queue. A busy game can delay frames; lower its GPU load or update the AMD driver."));
                        None
                    }
                }
            } else {
                if !config.yuv444 && crate::compute::enabled(tuning) {
                    warnings.set("encoder_conversion", "AMF compute conversion unavailable: the capture texture cannot be shared. Converting on the graphics queue; lower game GPU load or update the AMD driver if frames are delayed.");
                }
                None
            };
            let created = match crate::amf::Encoder::new_gpu(
                config,
                image.gpu.clone(),
                tuning,
                compute.clone(),
            ) {
                Err(error) if compute.is_some() => {
                    warnings.set("encoder_conversion", format!("AMF compute queue initialization failed ({error:#}); retrying conversion on the graphics queue. Lower game GPU load or update the AMD driver if frames are delayed."));
                    crate::amf::Encoder::new_gpu(config, image.gpu.clone(), tuning, None)
                }
                created => created,
            };
            match created {
                Ok(mut encoder) => {
                    encoder.warnings = warnings.clone();
                    warnings.clear("encoder_fallback");
                    return Ok(Self::Amf(Box::new(encoder)));
                }
                Err(error) if !automatic => return Err(error),
                Err(error) => {
                    warnings.clear("encoder_conversion");
                    native_error = Some(("amf", error));
                }
            }
        }
        if let Some((backend, error)) = &native_error {
            tracing::warn!(error = %format!("{error:#}"), backend, "native encoder failed; trying other hardware encoders");
        }
        let fallback = |encoder: Self, native_error: Option<(&str, anyhow::Error)>| {
            if let Some((backend, error)) = native_error {
                warnings.set(
                    "encoder_fallback",
                    fallback_warning(backend, &format!("{error:#}"), encoder.backend()),
                );
            }
            encoder
        };
        let mut import_errors = Vec::new();
        if !config.yuv444
            && config.codec < 3
            && matches!(
                preference,
                "auto" | "" | "nvenc_legacy" | "quicksync" | "qsv"
            )
        {
            let codec = match config.codec {
                0 => "h264",
                1 => "hevc",
                _ => "av1",
            };
            let candidates: &[&str] = match preference {
                "nvenc_legacy" => &["nvenc"],
                "quicksync" | "qsv" => &["qsv"],
                _ => &["nvenc", "qsv"],
            };
            for suffix in candidates {
                let name = format!("{codec}_{suffix}");
                match Ffmpeg::new_gpu_options(config, &name, tuning, image) {
                    Ok(encoder) => {
                        tracing::info!(%name,"native D3D11 codec frame import enabled");
                        return Ok(fallback(Self::Ffmpeg(Box::new(encoder)), native_error));
                    }
                    Err(error) => {
                        import_errors.push(format!("{name}: {error:#}"));
                    }
                }
            }
        }
        let (encoder, native_error) = if native_error.is_some() {
            // The native encoder already failed on this GPU: go straight to
            // the other hardware encoders rather than opening it again.
            match Self::other_hardware(config, preference, tuning) {
                Ok(encoder) => (encoder, native_error),
                Err(mut others) => {
                    others.splice(0..0, import_errors);
                    return Err(refused(native_error, &others));
                }
            }
        } else {
            (
                Self::new_options(config, preference, &image.gpu.display.display_name, tuning)?,
                None,
            )
        };
        let encoder = fallback(encoder, native_error);
        if matches!(&encoder, Self::Ffmpeg(e) if e.native.is_none()) {
            warnings.set("encoder_readback", format!("Encoding with {} using CPU frame copies and colour conversion: {}. This can lower fps and increase latency; select AMF on AMD, or reduce resolution and frame rate.", encoder.backend(), if import_errors.is_empty() { "this codec/input mode has no native GPU import".into() } else { import_errors.join("; ") }));
        }
        if !encoder.hardware() {
            warnings.set("encoder_software", "Encoding in software because Software was selected. CPU encoding can miss the requested frame rate; select Automatic or AMF on AMD, or lower resolution and frame rate.");
        }
        Ok(encoder)
    }
    pub fn encode_gpu(
        &mut self,
        image: &GpuImage,
        idr: bool,
        bitrate: u32,
    ) -> Result<Vec<Encoded>> {
        if let Self::Amf(encoder) = self {
            return encoder.encode_gpu(image, idr, bitrate);
        }
        match self {
            Self::Ffmpeg(e) => e.encode_gpu(image, idr, bitrate),
            Self::Nvenc(e) => e.encode_gpu(image, idr, bitrate),
            Self::Pyrowave(e) => e.encode_gpu(image, idr, bitrate),
            Self::Amf(_) => unreachable!(),
        }
    }
    pub fn accepts_gpu_device(&self, image: &GpuImage) -> bool {
        use windows::core::Interface;
        match self {
            Self::Amf(e) => e.accepts_gpu_device(image),
            Self::Nvenc(e) => e.accepts_gpu_device(image),
            Self::Ffmpeg(e) => e
                .native
                .as_ref()
                .is_none_or(|n| n.device.device.as_raw() == image.gpu.device.as_raw()),
            Self::Pyrowave(e) => e.accepts_gpu_device(image),
        }
    }
    pub fn pending(&self) -> bool {
        match self {
            Self::Amf(e) => e.pending(),
            Self::Nvenc(e) => e.pending(),
            _ => false,
        }
    }
    /// Frames the GPU encoder has accepted and not yet finished.
    pub fn backlog(&self) -> usize {
        match self {
            Self::Amf(e) => e.backlog(),
            Self::Nvenc(e) => e.backlog(),
            _ => 0,
        }
    }
    pub fn log_stall(&self) {
        if let Self::Amf(encoder) = self {
            encoder.log_stall();
        }
    }
    pub fn device_removed(&self) -> Option<crate::device_loss::DeviceLost> {
        match self {
            Self::Amf(encoder) => encoder.device_removed(),
            _ => None,
        }
    }
    /// A GPU encoder, rather than FFmpeg's software codecs.
    pub fn hardware(&self) -> bool {
        match self {
            Self::Ffmpeg(e) => e.name.ends_with("_nvenc") || e.name.ends_with("_qsv"),
            _ => true,
        }
    }
    /// The encoder family, as the encoder setting names it.
    pub fn backend(&self) -> &'static str {
        match self {
            Self::Amf(_) => "amf",
            Self::Nvenc(_) => "nvenc",
            Self::Pyrowave(_) => "pyrowave",
            Self::Ffmpeg(e) if e.name.ends_with("_nvenc") => "nvenc_legacy",
            Self::Ffmpeg(e) if e.name.ends_with("_qsv") => "qsv",
            Self::Ffmpeg(_) => "software",
        }
    }
    pub fn supports_invalidation(&self) -> bool {
        match self {
            Self::Amf(e) => e.supports_invalidation(),
            Self::Nvenc(e) => e.supports_invalidation(),
            _ => false,
        }
    }
    pub fn invalidate_ref_frames(&mut self, first: u64, last: u64) -> bool {
        match self {
            Self::Amf(encoder) => encoder.invalidate_ref_frames(first, last),
            Self::Nvenc(encoder) => encoder.invalidate_ref_frames(first, last),
            _ => false,
        }
    }
    pub fn poll(&mut self) -> Result<Vec<Encoded>> {
        match self {
            Self::Amf(e) => e.poll(),
            Self::Nvenc(e) => e.poll(),
            _ => Ok(vec![]),
        }
    }
    /// A recreated encoder must keep the wire frame numbers used by RFI.
    pub fn set_next_frame(&mut self, frame: u64) {
        match self {
            Self::Amf(e) => e.set_next_frame(frame),
            Self::Nvenc(e) => e.set_next_frame(frame),
            _ => {}
        }
    }
    pub fn new(config: &Negotiated, preference: &str, display: &str) -> Result<Self> {
        Self::new_options(
            config,
            preference,
            display,
            &butterpollo_core::config::Config::default(),
        )
    }
    pub fn new_options(
        config: &Negotiated,
        preference: &str,
        display: &str,
        tuning: &butterpollo_core::config::Config,
    ) -> Result<Self> {
        let preference = butterpollo_core::encoder_policy::canonical_name(preference);
        // The GPU the encoding adapter setting names, as capture opens it;
        // the display's own GPU only when nothing is set. A Radeon beside an
        // NVIDIA card that drives the monitor must probe on the Radeon.
        let device = || {
            crate::capture::Device::new_adapter(
                display,
                tuning.get("adapter_name", ""),
                tuning.get("adapter_pnp_id", ""),
            )
        };
        if config.codec == 3 {
            return Ok(Self::Pyrowave(Box::new(
                crate::pyrowave::Encoder::new_device(config, device()?, tuning)?,
            )));
        }
        if matches!(preference, "nvenc" | "nvenc_experimental") {
            return Ok(Self::Nvenc(Box::new(
                crate::nvenc::Encoder::new_device_options(config, device()?, tuning)?,
            )));
        }
        if matches!(preference, "" | "auto") {
            let device = device().context("Automatic encoder selection could not open the configured GPU; check the adapter setting")?;
            let native_error = match native_for(&device) {
                Some("nvenc") => {
                    match crate::nvenc::Encoder::new_device_options(config, device, tuning) {
                        Ok(e) => return Ok(Self::Nvenc(Box::new(e))),
                        Err(error) => Some(("nvenc", error)),
                    }
                }
                Some(_) => match crate::amf::Encoder::new_device_options(config, device, tuning) {
                    Ok(e) => return Ok(Self::Amf(Box::new(e))),
                    Err(error) => Some(("amf", error)),
                },
                None => None,
            };
            return match Self::other_hardware(config, preference, tuning) {
                Ok(encoder) => {
                    if let Some((backend, error)) = native_error {
                        let message =
                            fallback_warning(backend, &format!("{error:#}"), encoder.backend());
                        tracing::warn!("{message}");
                    }
                    Ok(encoder)
                }
                Err(others) => Err(refused(native_error, &others)),
            };
        }
        if preference == "amf" {
            return Ok(Self::Amf(Box::new(
                crate::amf::Encoder::new_device_options(config, device()?, tuning)?,
            )));
        }
        Self::other_hardware(config, preference, tuning).map_err(|errors| {
            anyhow::anyhow!("unable to initialize encoder: {}", errors.join("; "))
        })
    }
    /// FFmpeg's encoders for `preference`; for Automatic, only its hardware
    /// ones (NVENC, Quick Sync). The errors when none starts.
    fn other_hardware(
        config: &Negotiated,
        preference: &str,
        tuning: &butterpollo_core::config::Config,
    ) -> std::result::Result<Self, Vec<String>> {
        let candidates =
            ffmpeg_candidates(config.codec, preference).map_err(|e| vec![e.to_string()])?;
        let mut errors = vec![];
        for name in candidates {
            match Ffmpeg::new_options(config, &name, tuning) {
                Ok(e) => return Ok(Self::Ffmpeg(Box::new(e))),
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        Err(errors)
    }
    pub fn encode(&mut self, image: &Image, idr: bool, bitrate: u32) -> Result<Vec<Encoded>> {
        match self {
            Self::Ffmpeg(e) => e.encode(image, idr, bitrate),
            Self::Amf(e) => e.encode(image, idr, bitrate),
            Self::Nvenc(e) => e.encode(image, idr, bitrate),
            Self::Pyrowave(e) => e.encode(image, idr, bitrate),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packet_slices_handle_empty_payloads_without_copying_nonempty_data() {
        let mut data = [1, 2, 3];
        // SAFETY: AVPacket is a C struct of integers and pointers that admit zero.
        let mut packet: ff::AVPacket = unsafe { std::mem::zeroed() };
        for (pointer, size) in [
            (ptr::null_mut(), 0),
            (data.as_mut_ptr(), 0),
            (ptr::null_mut(), data.len() as i32),
        ] {
            packet.data = pointer;
            packet.size = size;
            // SAFETY: Each payload is null or a zero-length borrow of live data.
            assert!(unsafe { packet_bytes(&packet) }.is_empty());
        }
        packet.data = data.as_mut_ptr();
        packet.size = data.len() as i32;
        // SAFETY: The packet borrows all of `data`, which remains live and unchanged.
        let bytes = unsafe { packet_bytes(&packet) };
        assert_eq!(bytes, data);
        assert_eq!(bytes.as_ptr(), data.as_ptr());
    }
    #[test]
    fn software_converter_rejects_zero_sized_images() {
        for pixel in [
            ff::AVPixelFormat_AV_PIX_FMT_YUV420P,
            ff::AVPixelFormat_AV_PIX_FMT_YUV420P10LE,
        ] {
            let mut convert = Convert::new(8, 4, pixel).unwrap();
            for (width, height) in [(0, 0), (0, 4), (8, 0)] {
                let image = Image {
                    width,
                    height,
                    stride: width as usize * 4,
                    bytes: vec![],
                    captured: std::time::Instant::now(),
                    pixel: crate::capture::Pixel::Bgra8,
                };
                assert_eq!(
                    convert.convert(&image).unwrap_err().to_string(),
                    "invalid captured image layout"
                );
            }
        }
    }
    #[test]
    fn software_converter_pads_source_planes_after_resizing() {
        for pixel in [
            ff::AVPixelFormat_AV_PIX_FMT_YUV420P,
            ff::AVPixelFormat_AV_PIX_FMT_YUV420P10LE,
        ] {
            let mut convert = Convert::new(8, 4, pixel).unwrap();
            for (width, height) in [(8, 4), (2, 4), (8, 4)] {
                let stride = width as usize * 4 + 4;
                let image = Image {
                    width,
                    height,
                    stride,
                    bytes: vec![255; stride * height as usize],
                    captured: std::time::Instant::now(),
                    pixel: crate::capture::Pixel::Bgra8,
                };
                convert.convert(&image).unwrap();
                let size = if convert.hdr {
                    convert.content.0 as usize * convert.content.1 as usize * 8
                } else {
                    assert_eq!(&convert.scratch[..image.bytes.len()], &image.bytes);
                    image.bytes.len()
                };
                assert_eq!(
                    convert.scratch.len(),
                    size + ff::AV_INPUT_BUFFER_PADDING_SIZE as usize
                );
                assert!(convert.scratch[size..].iter().all(|&byte| byte == 0));
            }
        }
    }
    #[test]
    fn automatic_never_selects_a_software_codec() {
        for codec in 0..=2 {
            for preference in ["", "auto", "nvenc_legacy", "qsv"] {
                assert!(
                    ffmpeg_candidates(codec, preference)
                        .unwrap()
                        .iter()
                        .all(|name| name.ends_with("_nvenc") || name.ends_with("_qsv"))
                );
            }
            assert!(ffmpeg_candidates(codec, "software").unwrap()[0].starts_with("lib"));
        }
        let error = refused(
            Some(("amf", anyhow::anyhow!("driver rejected mode"))),
            &["h264_qsv: not found".into()],
        );
        let message = format!("{error:#}");
        assert!(message.contains("AMF failed and other hardware encoders failed: h264_qsv"));
        assert!(message.contains("never encodes in software"));
        assert!(message.contains("driver rejected mode"));
        let warning = fallback_warning("amf", "driver rejected mode", "qsv");
        assert!(warning.starts_with("AMF could not start (driver rejected mode)"));
        assert!(warning.contains("encoding with qsv instead"));
    }
    #[test]
    fn automatic_picks_the_native_encoder_by_vendor() {
        // The Steam Deck and FirePro adapters are AMD without "Radeon".
        assert_eq!(native_backend(0x1002, "AMD Custom GPU 0405"), Some("amf"));
        assert_eq!(native_backend(0x1002, "AMD FirePro W7100"), Some("amf"));
        assert_eq!(native_backend(0x10de, "Quadro RTX 4000"), Some("nvenc"));
        assert_eq!(native_backend(0, "AMD Radeon RX 7900 XT"), Some("amf"));
        assert_eq!(native_backend(0, "NVIDIA GeForce RTX 4090"), Some("nvenc"));
        assert_eq!(native_backend(0x8086, "Intel(R) Arc(TM) A770"), None);
    }
    /// Needs the packaged FFmpeg libraries on PATH.
    #[test]
    #[ignore = "requires the packaged FFmpeg libraries on PATH"]
    fn native_software_converter_letterboxes_other_shapes() {
        let image = Image {
            width: 32,
            height: 32,
            stride: 128,
            bytes: vec![255; 32 * 32 * 4],
            captured: std::time::Instant::now(),
            pixel: crate::capture::Pixel::Bgra8,
        };
        let mut convert = Convert::new(64, 32, ff::AVPixelFormat_AV_PIX_FMT_YUV420P).unwrap();
        convert.convert(&image).unwrap();
        // SAFETY: Conversion initialized the owned 64x32 luma plane; every call below
        // uses x < 64 and y < 32, with the allocation's linesize and a live converter.
        let luma = |x: usize, y: usize| unsafe {
            *(*convert.frame).data[0].add(y * (*convert.frame).linesize[0] as usize + x)
        };
        for y in [0, 15, 31] {
            assert_eq!([luma(0, y), luma(15, y), luma(48, y), luma(63, y)], [16; 4]);
            assert_eq!([luma(16, y), luma(32, y), luma(47, y)], [235; 3]);
        }
        // SAFETY: The live YUV420P frame has an initialized 32x16 chroma plane, so
        // byte 2 is inside that allocation.
        let chroma = unsafe { *(*convert.frame).data[1].add(2) };
        assert_eq!(chroma, 128);
    }
    /// Enumerates native adapters before rejecting a nonexistent GPU.
    #[test]
    #[ignore = "requires an interactive desktop display and a hardware GPU"]
    fn encoders_open_on_the_configured_gpu() {
        let stream = Negotiated {
            width: 640,
            height: 480,
            fps: 30,
            bitrate_kbps: 2000,
            ..Default::default()
        };
        // A GPU that is not there: the encoder must not quietly use the
        // display's GPU instead, as the codec probe did with two GPUs.
        let tuning =
            butterpollo_core::config::Config::parse("adapter_name = No Such GPU\n").unwrap();
        for preference in ["amf", "nvenc"] {
            let error = Encoder::new_options(&stream, preference, "", &tuning)
                .err()
                .expect("an encoder on a missing GPU");
            assert!(format!("{error:#}").contains("No Such GPU"), "{error:#}");
        }
    }
}
