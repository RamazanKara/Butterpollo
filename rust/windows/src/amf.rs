//! AMD's C ABI is called directly. No Butterpollo C++ code or FFmpeg AMF encoder is linked.
use crate::{
    amf_abi::*,
    capture::{Device, GpuImage, Image},
    encoder::{Convert, Encoded},
    ff,
};
use anyhow::{Context, Result, bail};
use std::{
    ptr,
    time::{Duration, Instant},
};
use windows::core::Interface;
pub(crate) fn check(n: AMF_RESULT) -> Result<()> {
    if n != AMF_RESULT_AMF_OK {
        bail!("AMF error {n}");
    }
    Ok(())
}
pub(crate) fn int(n: i64) -> AMFVariantStruct {
    AMFVariantStruct {
        type_: AMF_VARIANT_TYPE_AMF_VARIANT_INT64,
        __bindgen_anon_1: AMFVariantStruct__bindgen_ty_1 { int64Value: n },
    }
}
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
struct Submission {
    pts: i64,
    started: Instant,
    _capture: Option<GpuImage>,
    _converted: Option<std::sync::Arc<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D>>,
}
pub struct Encoder {
    component: *mut AMFComponent,
    context: *mut AMFContext,
    convert: Option<Convert>,
    config: butterpollo_core::rtsp::Negotiated,
    codec: u8,
    _device: Device,
    _library: libloading::Library,
    index: i64,
    gpu_convert: Option<crate::amf_gpu::Converter>,
    in_flight: std::collections::VecDeque<Submission>,
    bitrate: u32,
}
impl Encoder {
    pub fn new(config: &butterpollo_core::rtsp::Negotiated, display: &str) -> Result<Self> {
        Self::new_device(config, Device::new(display)?)
    }
    pub fn new_device(config: &butterpollo_core::rtsp::Negotiated, device: Device) -> Result<Self> {
        if config.yuv444 {
            bail!("AMF does not expose 4:4:4 for this encoder; select NVENC or software");
        }
        if config.hdr && config.codec == 0 {
            bail!("H.264 does not support HDR10");
        }
        unsafe {
            let library =
                libloading::Library::new("amfrt64.dll").context("AMD AMF runtime unavailable")?;
            let init: libloading::Symbol<
                unsafe extern "C" fn(u64, *mut *mut AMFFactory) -> AMF_RESULT,
            > = library.get(b"AMFInit\0")?;
            let mut factory = ptr::null_mut();
            let version = ((AMF_VERSION_MAJOR as u64) << 48)
                | ((AMF_VERSION_MINOR as u64) << 32)
                | ((AMF_VERSION_RELEASE as u64) << 16)
                | AMF_VERSION_BUILD_NUM as u64;
            check(init(version, &mut factory))?;
            if factory.is_null() {
                bail!("AMF returned no factory");
            }
            let mut context = ptr::null_mut();
            check(((*(*factory).pVtbl).CreateContext.unwrap())(
                factory,
                &mut context,
            ))?;
            let mut component = ptr::null_mut();
            let result = (|| -> Result<()> {
                check(((*(*context).pVtbl).InitDX11.unwrap())(
                    context,
                    device.device.as_raw(),
                    AMF_DX_VERSION_AMF_DX11_1,
                ))
                .context("AMF InitDX11")?;
                let name = match config.codec {
                    0 => "AMFVideoEncoderVCE_AVC",
                    1 => "AMFVideoEncoderHW_HEVC",
                    2 => "AMFVideoEncoderHW_AV1",
                    _ => bail!("unsupported AMF codec"),
                };
                check(((*(*factory).pVtbl).CreateComponent.unwrap())(
                    factory,
                    context,
                    wide(name).as_ptr(),
                    &mut component,
                ))
                .with_context(|| format!("AMF CreateComponent {name}"))?;
                Ok(())
            })();
            if let Err(e) = result {
                if !component.is_null() {
                    ((*(*component).pVtbl).Release.unwrap())(component);
                }
                ((*(*context).pVtbl).Terminate.unwrap())(context);
                ((*(*context).pVtbl).Release.unwrap())(context);
                return Err(e);
            }
            let mut e = Self {
                component,
                context,
                convert: None,
                config: config.clone(),
                codec: config.codec,
                _device: device,
                _library: library,
                index: 0,
                gpu_convert: None,
                in_flight: std::collections::VecDeque::new(),
                bitrate: config.bitrate_kbps,
            };
            e.property("Usage", int(if config.codec == 2 { 2 } else { 1 }))?;
            e.property(
                "QualityPreset",
                int(match config.codec {
                    0 => 1,
                    1 => 10,
                    _ => 100,
                }),
            )?;
            e.property(
                "FrameSize",
                AMFVariantStruct {
                    type_: AMF_VARIANT_TYPE_AMF_VARIANT_SIZE,
                    __bindgen_anon_1: AMFVariantStruct__bindgen_ty_1 {
                        sizeValue: AMFSize {
                            width: config.width as i32,
                            height: config.height as i32,
                        },
                    },
                },
            )?;
            e.property(
                "FrameRate",
                AMFVariantStruct {
                    type_: AMF_VARIANT_TYPE_AMF_VARIANT_RATE,
                    __bindgen_anon_1: AMFVariantStruct__bindgen_ty_1 {
                        rateValue: AMFRate {
                            num: config.fps,
                            den: 1,
                        },
                    },
                },
            )?;
            e.property("TargetBitrate", int(i64::from(config.bitrate_kbps) * 1000))?;
            e.property(
                "RateControlMethod",
                int(if config.codec == 0 { 2 } else { 3 }),
            )?;
            let _ = e.property("BPicturesPattern", int(0));
            if config.codec == 1 {
                // Request keyframes and headers per surface: finite GOPs stall
                // recent VCN drivers, as established by the original backend.
                e.property("GOPSize", int(0))?;
            }
            if config.codec == 2 {
                // AMF defaults to 64x16 alignment, which rejects 1080-line
                // input. RDNA3 still pads that AV1 output to 1082 lines; exact
                // 1080-line streams require HEVC on that hardware.
                e.property(
                    "AlignmentMode",
                    int(AMF_VIDEO_ENCODER_AV1_ALIGNMENT_MODE_ENUM_AMF_VIDEO_ENCODER_AV1_ALIGNMENT_MODE_NO_RESTRICTIONS as i64),
                )?;
                e.property("GOPSize", int(0))?;
            }
            // A blocking query can inherit Windows' 15.6 ms scheduler tick.
            let _ = e.property("QueryTimeout", int(0));
            if config.hdr {
                e.property("ColorBitDepth", int(10))?;
                if config.codec == 1 {
                    e.property("Profile", int(2))?;
                }
                let prefix = if config.codec == 1 { "Hevc" } else { "Av1" };
                for (name, value) in if config.codec == 1 {
                    [
                        ("InColorProfile", 2),
                        ("OutColorProfile", 2),
                        ("InColorTransferChar", 16),
                        ("OutColorTransferChar", 16),
                        ("InColorPrimaries", 9),
                        ("OutColorPrimaries", 9),
                    ]
                } else {
                    [
                        ("InputColorProfile", 2),
                        ("OutputColorProfile", 2),
                        ("InputColorTransferChar", 16),
                        ("OutputColorTransferChar", 16),
                        ("InputColorPrimaries", 9),
                        ("OutputColorPrimaries", 9),
                    ]
                } {
                    e.property_raw(&format!("{prefix}{name}"), int(value))?;
                }
            }
            let _ = e.property(
                "PreAnalysisEnable",
                AMFVariantStruct {
                    type_: AMF_VARIANT_TYPE_AMF_VARIANT_BOOL,
                    __bindgen_anon_1: AMFVariantStruct__bindgen_ty_1 { boolValue: 0 },
                },
            );
            check(((*(*e.component).pVtbl).Init.unwrap())(
                e.component,
                if config.hdr {
                    AMF_SURFACE_FORMAT_AMF_SURFACE_P010
                } else {
                    AMF_SURFACE_FORMAT_AMF_SURFACE_NV12
                },
                config.width as i32,
                config.height as i32,
            ))
            .context("AMF encoder Init")?;
            Ok(e)
        }
    }
    fn property(&mut self, name: &str, value: AMFVariantStruct) -> Result<()> {
        let prefix = match self.codec {
            0 => "",
            1 => "Hevc",
            _ => "Av1",
        };
        let name = format!("{prefix}{name}");
        self.property_raw(&name, value)
    }
    fn property_raw(&mut self, name: &str, value: AMFVariantStruct) -> Result<()> {
        unsafe {
            check(((*(*self.component).pVtbl).SetProperty.unwrap())(
                self.component,
                wide(name).as_ptr(),
                value,
            ))
            .with_context(|| format!("AMF property {name}"))
        }
    }
    pub fn pending(&self) -> bool {
        !self.in_flight.is_empty()
    }
    pub fn poll(&mut self) -> Result<Vec<Encoded>> {
        unsafe {
            let mut output = vec![];
            loop {
                let mut data = ptr::null_mut();
                let code =
                    ((*(*self.component).pVtbl).QueryOutput.unwrap())(self.component, &mut data);
                if code == AMF_RESULT_AMF_REPEAT
                    || code == AMF_RESULT_AMF_EOF
                    || code == AMF_RESULT_AMF_NEED_MORE_INPUT
                {
                    break;
                }
                check(code)?;
                if data.is_null() {
                    break;
                }
                let buffer = data as *mut AMFBuffer;
                let pts = ((*(*data).pVtbl).GetPts.unwrap())(data);
                let latency = self
                    .in_flight
                    .iter()
                    .position(|s| s.pts == pts)
                    .and_then(|position| self.in_flight.remove(position))
                    .map(|s| s.started.elapsed());
                let v = &*(*buffer).pVtbl;
                let size = (v.GetSize.unwrap())(buffer);
                let raw = (v.GetNative.unwrap())(buffer) as *const u8;
                let mut picture: AMFVariantStruct = std::mem::zeroed();
                let prop = match self.codec {
                    0 => "OutputDataType",
                    1 => "HevcOutputDataType",
                    _ => "Av1OutputFrameType",
                };
                let _ = (v.GetProperty.unwrap())(buffer, wide(prop).as_ptr(), &mut picture);
                if size > 64 * 1024 * 1024 || raw.is_null() {
                    (v.Release.unwrap())(buffer);
                    bail!("invalid AMF output buffer");
                }
                let idr = if picture.type_ == AMF_VARIANT_TYPE_AMF_VARIANT_INT64 {
                    picture.__bindgen_anon_1.int64Value == 0
                } else {
                    self.index == 1
                };
                let bytes = std::slice::from_raw_parts(raw, size as usize).to_vec();
                (v.Release.unwrap())(buffer);
                output.push(Encoded {
                    bytes,
                    idr,
                    latency,
                });
            }
            Ok(output)
        }
    }
    pub fn encode(&mut self, image: &Image, idr: bool, bitrate: u32) -> Result<Vec<Encoded>> {
        let started = Instant::now();
        self.set_bitrate(bitrate)?;
        if self.convert.is_none() {
            // Native GPU sessions never allocate a CPU YUV frame or swscale
            // context. Allocate compatibility buffers only when used.
            self.convert = Some(Convert::new(
                self.config.width,
                self.config.height,
                if self.config.hdr {
                    ff::AVPixelFormat_AV_PIX_FMT_P010LE
                } else {
                    ff::AVPixelFormat_AV_PIX_FMT_NV12
                },
            )?);
        }
        let convert = self.convert.as_mut().unwrap();
        convert.convert(image)?;
        let frame = convert.frame;
        unsafe {
            let mut surface = ptr::null_mut();
            check(((*(*self.context).pVtbl).AllocSurface.unwrap())(
                self.context,
                AMF_MEMORY_TYPE_AMF_MEMORY_HOST,
                if self.config.hdr {
                    AMF_SURFACE_FORMAT_AMF_SURFACE_P010
                } else {
                    AMF_SURFACE_FORMAT_AMF_SURFACE_NV12
                },
                self.config.width as i32,
                self.config.height as i32,
                &mut surface,
            ))
            .context("AMF host surface allocation")?;
            let v = &*(*surface).pVtbl;
            let prepared = (|| -> Result<()> {
                (v.SetPts.unwrap())(surface, self.index);
                (v.SetDuration.unwrap())(surface, 10_000_000 / i64::from(self.config.fps));
                for plane_index in 0..2 {
                    let plane = (v.GetPlaneAt.unwrap())(surface, plane_index);
                    if plane.is_null() {
                        bail!("AMF plane missing");
                    }
                    let pv = &*(*plane).pVtbl;
                    let dst = (pv.GetNative.unwrap())(plane) as *mut u8;
                    let pitch = (pv.GetHPitch.unwrap())(plane) as usize;
                    let width = self.config.width as usize * if self.config.hdr { 2 } else { 1 };
                    let height = self.config.height as usize / if plane_index == 0 { 1 } else { 2 };
                    let src = (*frame).data[plane_index];
                    let stride = (*frame).linesize[plane_index] as usize;
                    if dst.is_null() || pitch < width {
                        bail!("invalid AMF host surface pitch");
                    }
                    for row in 0..height {
                        ptr::copy_nonoverlapping(
                            src.add(row * stride),
                            dst.add(row * pitch),
                            width,
                        );
                    }
                }
                if idr {
                    let prop = match self.codec {
                        0 => "ForcePictureType",
                        1 => "HevcForcePictureType",
                        _ => "Av1ForceFrameType",
                    };
                    check((v.SetProperty.unwrap())(
                        surface,
                        wide(prop).as_ptr(),
                        int(if self.codec == 2 { 1 } else { 2 }),
                    ))?;
                    let flag = AMFVariantStruct {
                        type_: AMF_VARIANT_TYPE_AMF_VARIANT_BOOL,
                        __bindgen_anon_1: AMFVariantStruct__bindgen_ty_1 { boolValue: 1 },
                    };
                    if self.codec == 1 {
                        check((v.SetProperty.unwrap())(
                            surface,
                            wide("HevcInsertHeader").as_ptr(),
                            flag,
                        ))
                        .context("AMF keyframe headers")?;
                    } else if self.codec == 0 {
                        check((v.SetProperty.unwrap())(
                            surface,
                            wide("InsertSPS").as_ptr(),
                            flag,
                        ))?;
                        check((v.SetProperty.unwrap())(
                            surface,
                            wide("InsertPPS").as_ptr(),
                            flag,
                        ))?;
                    }
                }
                Ok(())
            })();
            if let Err(e) = prepared {
                (v.Release.unwrap())(surface);
                return Err(e);
            }
            let mut output = vec![];
            let deadline = Instant::now() + Duration::from_millis(100);
            loop {
                let result = ((*(*self.component).pVtbl).SubmitInput.unwrap())(
                    self.component,
                    surface as *mut AMFData,
                );
                if result == AMF_RESULT_AMF_INPUT_FULL {
                    match self.poll() {
                        Ok(frames) => output.extend(frames),
                        Err(e) => {
                            (v.Release.unwrap())(surface);
                            return Err(e);
                        }
                    }
                    if Instant::now() > deadline {
                        (v.Release.unwrap())(surface);
                        bail!("AMF input queue failed to drain");
                    }
                    std::thread::yield_now();
                    continue;
                }
                let result = check(result);
                (v.Release.unwrap())(surface);
                result?;
                break;
            }
            self.in_flight.push_back(Submission {
                pts: self.index,
                started,
                _capture: None,
                _converted: None,
            });
            self.index += 1;
            output.extend(self.poll()?);
            Ok(output)
        }
    }
}
impl Encoder {
    fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        if bitrate != self.bitrate {
            self.property("TargetBitrate", int(i64::from(bitrate) * 1000))?;
            self.bitrate = bitrate;
        }
        Ok(())
    }
    fn wait_capacity(&mut self) -> Result<Vec<Encoded>> {
        let mut output = self.poll()?;
        let deadline = Instant::now() + Duration::from_millis(100);
        while self.in_flight.len() >= 8 {
            output.extend(self.poll()?);
            if Instant::now() >= deadline {
                bail!("AMF GPU queue failed to drain");
            }
            std::thread::yield_now();
        }
        Ok(output)
    }
    pub fn encode_gpu(
        &mut self,
        image: &GpuImage,
        idr: bool,
        bitrate: u32,
    ) -> Result<Vec<Encoded>> {
        let started = Instant::now();
        let mut output = self.wait_capacity()?;
        if self._device.device.as_raw() != image.gpu.device.as_raw() {
            bail!("GPU frame and encoder must use the same D3D11 device");
        }
        let source = (image.width, image.height, image.pixel);
        if self
            .gpu_convert
            .as_ref()
            .is_none_or(|converter| converter.source != source)
        {
            self.gpu_convert = Some(crate::amf_gpu::Converter::new(
                &self._device,
                &self.config,
                source,
            )?);
        }
        let (surface, converted) = self
            .gpu_convert
            .as_mut()
            .unwrap()
            .convert(self.context, image)?;
        self.set_bitrate(bitrate)?;
        unsafe {
            let v = &*(*surface.0).pVtbl;
            (v.SetPts.unwrap())(surface.0, self.index);
            (v.SetDuration.unwrap())(surface.0, 10_000_000 / i64::from(self.config.fps));
            if idr {
                let property = match self.codec {
                    0 => "ForcePictureType",
                    1 => "HevcForcePictureType",
                    _ => "Av1ForceFrameType",
                };
                check((v.SetProperty.unwrap())(
                    surface.0,
                    wide(property).as_ptr(),
                    int(if self.codec == 2 { 1 } else { 2 }),
                ))?;
                let headers: &[&str] = match self.codec {
                    0 => &["InsertSPS", "InsertPPS"],
                    1 => &["HevcInsertHeader"],
                    _ => &[],
                };
                for property in headers {
                    check((v.SetProperty.unwrap())(
                        surface.0,
                        wide(property).as_ptr(),
                        crate::amf_gpu::boolean(true),
                    ))?;
                }
            }
            let deadline = Instant::now() + Duration::from_millis(100);
            loop {
                let result = ((*(*self.component).pVtbl).SubmitInput.unwrap())(
                    self.component,
                    surface.0.cast(),
                );
                if result != AMF_RESULT_AMF_INPUT_FULL {
                    check(result)?;
                    break;
                }
                output.extend(self.poll()?);
                if Instant::now() >= deadline {
                    bail!("AMF GPU input queue failed to drain");
                }
                std::thread::yield_now();
            }
            // Native COM references alone cannot prevent our Arc pool from
            // reusing an in-flight capture texture. Retain the Arc until output.
            self.in_flight.push_back(Submission {
                pts: self.index,
                started,
                _capture: Some(image.clone()),
                _converted: Some(converted),
            });
            self.index += 1;
            output.extend(self.poll()?);
            Ok(output)
        }
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        // Destroy the converter before its context and dynamically loaded runtime.
        self.gpu_convert.take();
        unsafe {
            if !self.component.is_null() {
                ((*(*self.component).pVtbl).Terminate.unwrap())(self.component);
                ((*(*self.component).pVtbl).Release.unwrap())(self.component);
            }
            if !self.context.is_null() {
                ((*(*self.context).pVtbl).Terminate.unwrap())(self.context);
                ((*(*self.context).pVtbl).Release.unwrap())(self.context);
            }
        }
    }
}
