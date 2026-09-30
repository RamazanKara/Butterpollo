use crate::{capture::Image, ff};
use anyhow::{Result, bail};
use butterpollo_core::rtsp::Negotiated;
use std::{
    ffi::{CStr, CString},
    ptr,
};

pub struct Encoded {
    pub bytes: Vec<u8>,
    pub idr: bool,
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
    scratch: Vec<u8>,
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
            };
            check(ff::av_frame_get_buffer(s.frame, 32))?;
            Ok(s)
        }
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
            crate::color::hdr_rgba_scaled(image, self.width, self.height, &mut self.scratch);
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
                let matrix = ff::sws_getCoefficients(if self.hdr { 9 } else { 1 });
                check(ff::sws_setColorspaceDetails(
                    self.context,
                    matrix,
                    1,
                    matrix,
                    0,
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
            } else {
                ff::AVColorPrimaries_AVCOL_PRI_BT709
            };
            (*self.frame).color_trc = if self.hdr {
                ff::AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084
            } else {
                ff::AVColorTransferCharacteristic_AVCOL_TRC_BT709
            };
            (*self.frame).colorspace = if self.hdr {
                ff::AVColorSpace_AVCOL_SPC_BT2020_NCL
            } else {
                ff::AVColorSpace_AVCOL_SPC_BT709
            };
            (*self.frame).color_range = ff::AVColorRange_AVCOL_RANGE_MPEG;
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
    convert: Convert,
    index: i64,
    pub name: String,
}
impl Ffmpeg {
    pub fn new(config: &Negotiated, name: &str) -> Result<Self> {
        unsafe {
            let codec = ff::avcodec_find_encoder_by_name(c(name).as_ptr());
            if codec.is_null() {
                bail!("encoder {name} is unavailable in this SDK");
            }
            let hardware = name.ends_with("_nvenc") || name.ends_with("_qsv");
            if config.hdr && config.codec == 0 {
                bail!("H.264 cannot carry the negotiated HDR10 stream");
            }
            if name.ends_with("_qsv") && config.yuv444 {
                bail!("QSV 4:4:4 is unavailable in this SDK");
            }
            let context = ff::avcodec_alloc_context3(codec);
            if context.is_null() {
                bail!("cannot allocate encoder");
            }
            let pixel = if hardware {
                if config.yuv444 {
                    if config.hdr {
                        ff::AVPixelFormat_AV_PIX_FMT_YUV444P16LE
                    } else {
                        ff::AVPixelFormat_AV_PIX_FMT_YUV444P
                    }
                } else if config.hdr {
                    ff::AVPixelFormat_AV_PIX_FMT_P010LE
                } else {
                    ff::AVPixelFormat_AV_PIX_FMT_NV12
                }
            } else if config.hdr {
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
                (*context).pix_fmt = pixel;
                (*context).color_primaries = if config.hdr {
                    ff::AVColorPrimaries_AVCOL_PRI_BT2020
                } else {
                    ff::AVColorPrimaries_AVCOL_PRI_BT709
                };
                (*context).color_trc = if config.hdr {
                    ff::AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084
                } else {
                    ff::AVColorTransferCharacteristic_AVCOL_TRC_BT709
                };
                (*context).colorspace = if config.hdr {
                    ff::AVColorSpace_AVCOL_SPC_BT2020_NCL
                } else {
                    ff::AVColorSpace_AVCOL_SPC_BT709
                };
                (*context).color_range = ff::AVColorRange_AVCOL_RANGE_MPEG;
                (*context).time_base = ff::AVRational {
                    num: 1,
                    den: config.fps as i32,
                };
                (*context).framerate = ff::AVRational {
                    num: config.fps as i32,
                    den: 1,
                };
                (*context).bit_rate = i64::from(config.bitrate_kbps) * 1000;
                (*context).rc_max_rate = (*context).bit_rate;
                (*context).rc_buffer_size =
                    ((*context).bit_rate / config.fps as i64).max(1000) as i32;
                (*context).gop_size = i32::MAX;
                (*context).max_b_frames = 0;
                (*context).thread_count = if hardware { 1 } else { 2 };
                (*context).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as i32;
                let mut options = ptr::null_mut();
                let set = |options: &mut *mut ff::AVDictionary, key: &str, value: &str| {
                    ff::av_dict_set(options, c(key).as_ptr(), c(value).as_ptr(), 0);
                };
                if name.ends_with("_nvenc") {
                    set(&mut options, "preset", "p1");
                    set(&mut options, "tune", "ull");
                    set(&mut options, "rc", "cbr");
                    set(&mut options, "zerolatency", "1");
                    set(&mut options, "forced-idr", "1");
                    set(&mut options, "delay", "0");
                } else if name.ends_with("_qsv") {
                    set(&mut options, "preset", "veryfast");
                    set(&mut options, "async_depth", "1");
                    set(&mut options, "low_delay_brc", "1");
                    set(&mut options, "forced_idr", "1");
                } else if name == "libx264" {
                    set(&mut options, "preset", "ultrafast");
                    set(&mut options, "tune", "zerolatency");
                    set(
                        &mut options,
                        "x264-params",
                        "repeat-headers=1:annexb=1:scenecut=0",
                    );
                } else if name == "libx265" {
                    set(&mut options, "preset", "ultrafast");
                    set(&mut options, "tune", "zerolatency");
                    set(
                        &mut options,
                        "x265-params",
                        "repeat-headers=1:annexb=1:log-level=error",
                    );
                } else if name == "libsvtav1" {
                    set(&mut options, "preset", "12");
                    set(&mut options, "svtav1-params", "pred-struct=1:lookahead=0");
                }
                let result = ff::avcodec_open2(context, codec, &mut options);
                ff::av_dict_free(&mut options);
                check(result)
            };
            if let Err(e) = init {
                let mut p = context;
                ff::avcodec_free_context(&mut p);
                return Err(e);
            }
            let convert = match Convert::new(config.width, config.height, pixel) {
                Ok(c) => c,
                Err(e) => {
                    let mut p = context;
                    ff::avcodec_free_context(&mut p);
                    return Err(e);
                }
            };
            let packet = ff::av_packet_alloc();
            if packet.is_null() {
                let mut p = context;
                ff::avcodec_free_context(&mut p);
                bail!("cannot allocate encoded packet");
            }
            Ok(Self {
                context,
                packet,
                convert,
                index: 0,
                name: name.into(),
            })
        }
    }
    pub fn encode(&mut self, image: &Image, idr: bool, bitrate_kbps: u32) -> Result<Vec<Encoded>> {
        self.convert.convert(image)?;
        unsafe {
            (*self.context).bit_rate = i64::from(bitrate_kbps) * 1000;
            (*self.convert.frame).pts = self.index;
            (*self.convert.frame).pict_type = if idr {
                ff::AVPictureType_AV_PICTURE_TYPE_I
            } else {
                ff::AVPictureType_AV_PICTURE_TYPE_NONE
            };
            self.index += 1;
            check(ff::avcodec_send_frame(self.context, self.convert.frame))?;
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
    Ffmpeg(Ffmpeg),
    Amf(crate::amf::Encoder),
    Pyrowave(crate::pyrowave::Encoder),
}
impl Encoder {
    pub fn new(config: &Negotiated, preference: &str, display: &str) -> Result<Self> {
        if config.codec == 3 {
            return Ok(Self::Pyrowave(crate::pyrowave::Encoder::new(
                config, display,
            )?));
        }
        let codec = match config.codec {
            0 => "h264",
            1 => "hevc",
            2 => "av1",
            _ => bail!("unsupported codec"),
        };
        if preference == "amf" {
            return Ok(Self::Amf(crate::amf::Encoder::new(config, display)?));
        }
        let candidates: Vec<String> = match preference {
            "nvenc" => vec![format!("{codec}_nvenc")],
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
            match Ffmpeg::new(config, &name) {
                Ok(e) => return Ok(Self::Ffmpeg(e)),
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        if preference.is_empty() || preference == "auto" {
            match crate::amf::Encoder::new(config, display) {
                Ok(e) => return Ok(Self::Amf(e)),
                Err(e) => errors.push(format!("AMF: {e}")),
            }
            let name = match config.codec {
                0 => "libx264",
                1 => "libx265",
                _ => "libsvtav1",
            };
            if let Ok(e) = Ffmpeg::new(config, name) {
                return Ok(Self::Ffmpeg(e));
            }
        }
        bail!("unable to initialize encoder: {}", errors.join("; "))
    }
    pub fn encode(&mut self, image: &Image, idr: bool, bitrate: u32) -> Result<Vec<Encoded>> {
        match self {
            Self::Ffmpeg(e) => e.encode(image, idr, bitrate),
            Self::Amf(e) => e.encode(image, idr, bitrate),
            Self::Pyrowave(e) => e.encode(image, idr, bitrate),
        }
    }
}
