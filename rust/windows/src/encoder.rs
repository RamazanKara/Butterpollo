use crate::{
    capture::{GpuImage, Image},
    ff,
};
use anyhow::{Result, bail};
use butterpollo_core::rtsp::Negotiated;
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
    unsafe {
        ff::av_strerror(code, text.as_mut_ptr(), text.len());
    }
    bail!(
        "FFmpeg: {}",
        unsafe { CStr::from_ptr(text.as_ptr()) }.to_string_lossy()
    )
}
pub(crate) fn c(s: &str) -> CString {
    CString::new(s).expect("internal codec string contains NUL")
}
pub struct Convert {
    pub(crate) frame: *mut ff::AVFrame,
    context: *mut ff::SwsContext,
    width: u32,
    height: u32,
    pixel: i32,
    source: (u32, u32),
    hdr: bool,
    matrix: u8,
    full_range: bool,
    scratch: Vec<u8>,
    pub(crate) luminance: [f32; 2],
}
impl Convert {
    pub fn new(width: u32, height: u32, pixel: i32) -> Result<Self> {
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
        if image.stride < image.width as usize * pixel_bytes
            || image.bytes.len() < image.stride * image.height as usize
        {
            bail!("invalid captured image layout");
        }
        if self.hdr {
            crate::color::hdr_rgba_scaled_luminance(
                image,
                self.width,
                self.height,
                self.luminance,
                &mut self.scratch,
            );
        } else if image.pixel != crate::capture::Pixel::Bgra8 {
            bail!("HDR surface supplied to an SDR converter");
        }
        unsafe {
            check(ff::av_frame_make_writable(self.frame))?;
            let source = if self.hdr {
                (self.width, self.height)
            } else {
                (image.width, image.height)
            };
            if self.context.is_null() || self.source != source {
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
                    self.width as i32,
                    self.height as i32,
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
            }
            let src = [
                if self.hdr {
                    self.scratch.as_ptr()
                } else {
                    image.bytes.as_ptr()
                },
                ptr::null(),
                ptr::null(),
                ptr::null(),
            ];
            let strides = [
                if self.hdr {
                    self.width as i32 * 8
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
            check(ff::sws_scale(
                self.context,
                src.as_ptr(),
                strides.as_ptr(),
                0,
                self.source.1 as i32,
                (*self.frame).data.as_ptr(),
                (*self.frame).linesize.as_ptr(),
            ))?;
            Ok(())
        }
    }
}
impl Drop for Convert {
    fn drop(&mut self) {
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
    native: Option<crate::ffmpeg_gpu::Native>,
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
                .map(|image| crate::ffmpeg_gpu::Native::new(image, config, name.ends_with("_qsv")))
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
                let code = ff::avcodec_receive_packet(self.context, self.packet);
                if code == -11 || code == -541478725 {
                    break;
                }
                check(code)?;
                let n = (*self.packet).size;
                if !(0..=64 * 1024 * 1024).contains(&n) {
                    bail!("invalid encoder output size");
                }
                let bytes = std::slice::from_raw_parts((*self.packet).data, n as usize).to_vec();
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
        unsafe {
            ff::av_packet_free(&mut self.packet);
            ff::avcodec_free_context(&mut self.context);
        }
    }
}
pub enum Encoder {
    Ffmpeg(Box<Ffmpeg>),
    Amf(Box<crate::amf::Encoder>),
    Nvenc(Box<crate::nvenc::Encoder>),
    Pyrowave(Box<crate::pyrowave::Encoder>),
}
impl Encoder {
    pub fn set_hdr_metadata(&mut self, metadata: butterpollo_core::hdr::Metadata) {
        if let Self::Nvenc(encoder) = self {
            encoder.set_hdr_metadata(metadata);
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
        let preference = butterpollo_core::encoder_policy::canonical_name(preference);
        if config.codec == 3 {
            return Ok(Self::Pyrowave(Box::new(
                crate::pyrowave::Encoder::new_device(config, image.gpu.clone(), tuning)?,
            )));
        }
        if config.codec < 3
            && (matches!(preference, "nvenc" | "nvenc_experimental")
                || (matches!(preference, "" | "auto")
                    && image
                        .gpu
                        .display
                        .adapter
                        .to_ascii_lowercase()
                        .contains("nvidia")))
        {
            match crate::nvenc::Encoder::new_device_options(config, image.gpu.clone(), tuning) {
                Ok(encoder) => return Ok(Self::Nvenc(Box::new(encoder))),
                Err(error) if matches!(preference, "nvenc" | "nvenc_experimental") => {
                    return Err(error);
                }
                Err(error) => {
                    tracing::warn!(%error, "Native NVENC unavailable; trying compatible encoders")
                }
            }
        }
        if config.codec != 3
            && (preference == "amf"
                || ((preference.is_empty() || preference == "auto")
                    && image.gpu.display.adapter.contains("Radeon")))
        {
            match crate::amf::Encoder::new_device_options(config, image.gpu.clone(), tuning) {
                Ok(encoder) => return Ok(Self::Amf(Box::new(encoder))),
                Err(error) if preference == "amf" => return Err(error),
                Err(error) => tracing::warn!(%error, "AMF unavailable; trying other encoders"),
            }
        }
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
                        return Ok(Self::Ffmpeg(Box::new(encoder)));
                    }
                    Err(error) => {
                        tracing::debug!(%error,%name,"native codec import unavailable; trying compatible input")
                    }
                }
            }
        }
        Self::new_options(config, preference, &image.gpu.display.display_name, tuning)
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
        if config.codec == 3 {
            return Ok(Self::Pyrowave(Box::new(
                crate::pyrowave::Encoder::new_device(
                    config,
                    crate::capture::Device::new(display)?,
                    tuning,
                )?,
            )));
        }
        if matches!(preference, "nvenc" | "nvenc_experimental") {
            return Ok(Self::Nvenc(Box::new(
                crate::nvenc::Encoder::new_device_options(
                    config,
                    crate::capture::Device::new(display)?,
                    tuning,
                )?,
            )));
        }
        if matches!(preference, "" | "auto")
            && let Ok(device) = crate::capture::Device::new(display)
            && device
                .display
                .adapter
                .to_ascii_lowercase()
                .contains("nvidia")
        {
            match crate::nvenc::Encoder::new_device_options(config, device, tuning) {
                Ok(e) => return Ok(Self::Nvenc(Box::new(e))),
                Err(error) => {
                    tracing::warn!(%error, "Native NVENC unavailable; trying compatible encoders")
                }
            }
        }
        let codec = match config.codec {
            0 => "h264",
            1 => "hevc",
            2 => "av1",
            _ => bail!("unsupported codec"),
        };
        if preference == "amf" {
            return Ok(Self::Amf(Box::new(
                crate::amf::Encoder::new_device_options(
                    config,
                    crate::capture::Device::new(display)?,
                    tuning,
                )?,
            )));
        }
        let candidates: Vec<String> = match preference {
            "nvenc_legacy" => vec![format!("{codec}_nvenc")],
            "quicksync" | "qsv" => vec![format!("{codec}_qsv")],
            "software" => vec![
                match config.codec {
                    0 => "libx264",
                    1 => "libx265",
                    _ => "libsvtav1",
                }
                .into(),
            ],
            _ => vec![format!("{codec}_nvenc"), format!("{codec}_qsv")],
        };
        let mut errors = vec![];
        for name in candidates {
            match Ffmpeg::new_options(config, &name, tuning) {
                Ok(e) => return Ok(Self::Ffmpeg(Box::new(e))),
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        if preference.is_empty() || preference == "auto" {
            match crate::amf::Encoder::new_device_options(
                config,
                crate::capture::Device::new(display)?,
                tuning,
            ) {
                Ok(e) => return Ok(Self::Amf(Box::new(e))),
                Err(e) => errors.push(format!("AMF: {e}")),
            }
            let name = match config.codec {
                0 => "libx264",
                1 => "libx265",
                _ => "libsvtav1",
            };
            if let Ok(e) = Ffmpeg::new_options(config, name, tuning) {
                return Ok(Self::Ffmpeg(Box::new(e)));
            }
        }
        bail!("unable to initialize encoder: {}", errors.join("; "))
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
