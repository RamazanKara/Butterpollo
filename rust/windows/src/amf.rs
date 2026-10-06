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
    presentation: Instant,
    after_invalidation: bool,
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
    references: butterpollo_core::ltr::References,
    ownership: Box<crate::amf_gpu::Ownership>,
    pub(crate) luminance: [f32; 2],
    /// The HDR10 metadata last written into the bitstream.
    hdr_metadata: Option<butterpollo_core::hdr::Metadata>,
    /// Conversion on a D3D12 compute queue, with AMF on D3D12.
    compute: Option<Box<ComputeInput>>,
}
struct ComputeInput {
    converter: crate::compute::Converter,
    context: crate::amf_gpu::D3d12Context,
}
impl Encoder {
    pub fn new(config: &butterpollo_core::rtsp::Negotiated, display: &str) -> Result<Self> {
        Self::new_device(config, Device::new(display)?)
    }
    pub fn new_device(config: &butterpollo_core::rtsp::Negotiated, device: Device) -> Result<Self> {
        Self::new_device_options(config, device, &butterpollo_core::config::Config::default())
    }
    pub fn new_device_options(
        config: &butterpollo_core::rtsp::Negotiated,
        device: Device,
        options: &butterpollo_core::config::Config,
    ) -> Result<Self> {
        Self::new_device_alignment_options(
            config,
            device,
            options,
            AMF_VIDEO_ENCODER_AV1_ALIGNMENT_MODE_ENUM_AMF_VIDEO_ENCODER_AV1_ALIGNMENT_MODE_NO_RESTRICTIONS as i64,
        )
    }
    fn new_device_alignment_options(
        config: &butterpollo_core::rtsp::Negotiated,
        device: Device,
        options: &butterpollo_core::config::Config,
        av1_alignment: i64,
    ) -> Result<Self> {
        Self::create(config, device, options, av1_alignment, None)
    }
    /// An encoder for captured GPU images. With `compute`, the colours are
    /// converted on that D3D12 compute queue and AMF encodes from D3D12, so
    /// neither waits behind a game's work on the graphics engine.
    pub fn new_gpu(
        config: &butterpollo_core::rtsp::Negotiated,
        device: Device,
        options: &butterpollo_core::config::Config,
        compute: Option<std::sync::Arc<crate::compute::Compute>>,
    ) -> Result<Self> {
        Self::create(
            config,
            device,
            options,
            AMF_VIDEO_ENCODER_AV1_ALIGNMENT_MODE_ENUM_AMF_VIDEO_ENCODER_AV1_ALIGNMENT_MODE_NO_RESTRICTIONS as i64,
            compute,
        )
    }
    fn create(
        config: &butterpollo_core::rtsp::Negotiated,
        device: Device,
        options: &butterpollo_core::config::Config,
        av1_alignment: i64,
        compute: Option<std::sync::Arc<crate::compute::Compute>>,
    ) -> Result<Self> {
        let mut effective_config = config.clone();
        effective_config.intra_refresh =
            butterpollo_core::encoder_policy::amf_intra_refresh(config);
        if config.intra_refresh && !effective_config.intra_refresh {
            tracing::warn!(
                client_max_reference_frames = config.references,
                "AMF H.264 intra refresh requires two references; using IDR recovery for this decoder"
            );
        }
        let config = &effective_config;
        if config.yuv444 {
            bail!("AMF does not expose 4:4:4 for this encoder; select NVENC or software");
        }
        if config.ten_bit() && config.codec == 0 {
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
            let mut d3d12 = None;
            let result = (|| -> Result<()> {
                if let Some(compute) = &compute {
                    let interface = crate::amf_gpu::D3d12Context::new(context)?;
                    interface.init(&compute.device)?;
                    d3d12 = Some(interface);
                } else {
                    check(((*(*context).pVtbl).InitDX11.unwrap())(
                        context,
                        device.device.as_raw(),
                        AMF_DX_VERSION_AMF_DX11_1,
                    ))
                    .context("AMF InitDX11")?;
                }
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
                drop(d3d12);
                if !component.is_null() {
                    ((*(*component).pVtbl).Release.unwrap())(component);
                }
                ((*(*context).pVtbl).Terminate.unwrap())(context);
                ((*(*context).pVtbl).Release.unwrap())(context);
                return Err(e);
            }
            let compute = match (d3d12, compute) {
                (Some(interface), Some(compute)) => Some(Box::new(ComputeInput {
                    converter: crate::compute::Converter::new(
                        compute,
                        config.width,
                        config.height,
                        config.ten_bit(),
                    )?,
                    context: interface,
                })),
                _ => None,
            };
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
                references: Default::default(),
                ownership: crate::amf_gpu::Ownership::new(),
                luminance: [100., 1.],
                hdr_metadata: None,
                compute,
            };
            for property in butterpollo_core::encoder_policy::amf(options, config)? {
                use butterpollo_core::encoder_policy::Value;
                let value = match property.value {
                    Value::Integer(n) => int(n),
                    Value::Boolean(on) => AMFVariantStruct {
                        type_: AMF_VARIANT_TYPE_AMF_VARIANT_BOOL,
                        __bindgen_anon_1: AMFVariantStruct__bindgen_ty_1 {
                            boolValue: u8::from(on),
                        },
                    },
                };
                let result = e.property_raw(&property.name, value).and_then(|()| {
                    let mut applied = int(0);
                    check(((*(*e.component).pVtbl).GetProperty.unwrap())(
                        e.component,
                        wide(&property.name).as_ptr(),
                        &mut applied,
                    ))?;
                    let matches = match property.value {
                        Value::Integer(n) => {
                            applied.type_ == AMF_VARIANT_TYPE_AMF_VARIANT_INT64
                                && (applied.__bindgen_anon_1.int64Value == n
                                    || (property.name == "Av1NumTilesPerFrame"
                                        && applied.__bindgen_anon_1.int64Value > 0))
                        }
                        Value::Boolean(on) => match applied.type_ {
                            AMF_VARIANT_TYPE_AMF_VARIANT_BOOL => {
                                (applied.__bindgen_anon_1.boolValue != 0) == on
                            }
                            AMF_VARIANT_TYPE_AMF_VARIANT_INT64 => {
                                (applied.__bindgen_anon_1.int64Value != 0) == on
                            }
                            _ => false,
                        },
                    };
                    if !matches {
                        bail!(
                            "AMF did not apply {} = {:?} (reported variant {}, integer {})",
                            property.name,
                            property.value,
                            applied.type_,
                            if applied.type_ == AMF_VARIANT_TYPE_AMF_VARIANT_INT64 {
                                applied.__bindgen_anon_1.int64Value
                            } else {
                                -1
                            }
                        );
                    }
                    Ok(())
                });
                if let Err(error) = result {
                    if property.required {
                        return Err(error)
                            .with_context(|| format!("AMF setting {}", property.name));
                    }
                    tracing::warn!(%error, setting=%property.name, "AMF setting unavailable; retaining driver default");
                }
            }
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
                            num: config.fps_millihz(),
                            den: 1000,
                        },
                    },
                },
            )?;
            e.property("TargetBitrate", int(i64::from(config.bitrate_kbps) * 1000))?;
            let _ = e.property("BPicturesPattern", int(0));
            if config.codec == 1 {
                // Request keyframes and headers per surface: finite GOPs stall
                // recent VCN drivers, as established by the original backend.
                e.property("GOPSize", int(0))?;
            }
            if config.codec == 2 {
                e.property("AlignmentMode", int(av1_alignment))?;
                e.property("GOPSize", int(0))?;
            }
            // QueryOutput waits in the driver and returns the moment a frame is
            // encoded; no host timer is involved. `poll` only queries while a
            // frame is in flight, so an expired wait never stalls an idle loop.
            let _ = e.property("QueryTimeout", int(1));
            if config.ten_bit() {
                e.property("ColorBitDepth", int(10))?;
                if config.codec == 1 {
                    e.property("Profile", int(2))?;
                }
            }
            let matrix = config.color_matrix();
            let profile = if config.full_range() {
                match matrix {
                    0 => 3,
                    1 => 7,
                    _ => 8,
                }
            } else {
                i64::from(matrix)
            };
            let primaries = match matrix {
                0 => 6,
                2 => 9,
                _ => 1,
            };
            let transfer = if config.hdr {
                16
            } else {
                match matrix {
                    0 => 6,
                    2 => 14,
                    _ => 1,
                }
            };
            let prefix = match config.codec {
                0 => "",
                1 => "Hevc",
                _ => "Av1",
            };
            let (input, output) = if config.codec == 2 {
                ("Input", "Output")
            } else {
                ("In", "Out")
            };
            for (suffix, value) in [
                ("ColorProfile", profile),
                ("ColorTransferChar", transfer),
                ("ColorPrimaries", primaries),
            ] {
                e.property_raw(&format!("{prefix}{input}{suffix}"), int(value))?;
                e.property_raw(&format!("{prefix}{output}{suffix}"), int(value))?;
            }
            let full = AMFVariantStruct {
                type_: AMF_VARIANT_TYPE_AMF_VARIANT_BOOL,
                __bindgen_anon_1: AMFVariantStruct__bindgen_ty_1 {
                    boolValue: u8::from(config.full_range()),
                },
            };
            e.property_raw(&format!("{prefix}InputFullRangeColor"), full)?;
            // The range written into the bitstream. HEVC and AV1 default to
            // limited, which a full-range stream must not claim.
            e.property_raw(
                match config.codec {
                    0 => "FullRangeColor",
                    1 => "HevcNominalRange",
                    _ => "Av1NominalRange",
                },
                full,
            )?;
            let requested_ltr = options.integer("amd_ltr_frames", 0).clamp(0, 4) as usize;
            let ltr_count = butterpollo_core::encoder_policy::amf_ltr_frames(options, config);
            if ltr_count < requested_ltr {
                tracing::info!(
                    requested_ltr,
                    client_max_reference_frames = config.references,
                    effective_ltr_frames = ltr_count,
                    intra_refresh = config.intra_refresh,
                    "AMF LTR anchors limited by decoder reference budget or intra refresh"
                );
            }
            e.configure_ltr(ltr_count);
            check(((*(*e.component).pVtbl).Init.unwrap())(
                e.component,
                if config.ten_bit() {
                    AMF_SURFACE_FORMAT_AMF_SURFACE_P010
                } else {
                    AMF_SURFACE_FORMAT_AMF_SURFACE_NV12
                },
                config.width as i32,
                config.height as i32,
            ))
            .context("AMF encoder Init")?;
            e.log_effective();
            Ok(e)
        }
    }
    /// An integer or boolean property as the driver now has it.
    fn read(&self, name: &str) -> Option<i64> {
        unsafe {
            let mut value = int(0);
            if ((*(*self.component).pVtbl).GetProperty.unwrap())(
                self.component,
                wide(name).as_ptr(),
                &mut value,
            ) != AMF_RESULT_AMF_OK
            {
                return None;
            }
            match value.type_ {
                AMF_VARIANT_TYPE_AMF_VARIANT_INT64 => Some(value.__bindgen_anon_1.int64Value),
                AMF_VARIANT_TYPE_AMF_VARIANT_BOOL => {
                    Some(i64::from(value.__bindgen_anon_1.boolValue))
                }
                _ => None,
            }
        }
    }
    /// The settings that decide encode time, as the driver applied them.
    fn log_effective(&self) {
        let names: &[&str] = match self.codec {
            0 => &[
                "Usage",
                "QualityPreset",
                "RateControlMethod",
                "LowLatencyInternal",
                "RateControlPreanalysisEnable",
                "EnableVBAQ",
                "SlicesPerFrame",
                "InputQueueSize",
                "QueryTimeout",
                "FullRangeColor",
                "MaxNumRefFrames",
                "MaxOfLTRFrames",
            ],
            1 => &[
                "HevcUsage",
                "HevcQualityPreset",
                "HevcRateControlMethod",
                "LowLatencyInternal",
                "HevcMultiHwInstanceEncode",
                "HevcRateControlPreAnalysisEnable",
                "HevcEnableVBAQ",
                "HevcSlicesPerFrame",
                "HevcInputQueueSize",
                "HevcQueryTimeout",
                "HevcNominalRange",
                "HevcMaxNumRefFrames",
                "HevcMaxOfLTRFrames",
            ],
            _ => &[
                "Av1Usage",
                "Av1QualityPreset",
                "Av1RateControlMethod",
                "Av1EncodingLatencyMode",
                "Av1MultiHwInstanceEncode",
                "Av1RateControlPreEncode",
                "Av1AQMode",
                "Av1NumTilesPerFrame",
                "Av1InputQueueSize",
                "Av1QueryTimeout",
                "Av1NominalRange",
                "Av1MaxNumRefFrames",
                "Av1MaxNumLTRFrames",
            ],
        };
        let mut settings: Vec<String> = names
            .iter()
            .map(|name| match self.read(name) {
                Some(value) => format!("{name}={value}"),
                None => format!("{name}=?"),
            })
            .collect();
        unsafe {
            let mut caps = ptr::null_mut();
            if ((*(*self.component).pVtbl).GetCaps.unwrap())(self.component, &mut caps)
                == AMF_RESULT_AMF_OK
                && !caps.is_null()
            {
                let names = match self.codec {
                    0 => ["NumOfHwInstances", "ColorConversion"],
                    1 => ["HevcNumOfHwInstances", "HevcColorConversion"],
                    _ => ["Av1CapNumOfHwInstances", "Av1CapColorConversion"],
                };
                for name in names {
                    let mut value = int(0);
                    if ((*(*caps).pVtbl).GetProperty.unwrap())(
                        caps,
                        wide(name).as_ptr(),
                        &mut value,
                    ) == AMF_RESULT_AMF_OK
                        && value.type_ == AMF_VARIANT_TYPE_AMF_VARIANT_INT64
                    {
                        settings.push(format!("{name}={}", value.__bindgen_anon_1.int64Value));
                    }
                }
                ((*(*caps).pVtbl).Release.unwrap())(caps);
            }
        }
        tracing::info!(
            settings = %settings.join(" "),
            client_max_reference_frames = self.config.references,
            "AMF encoder settings"
        );
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
    /// Write HDR10 static metadata (the mastering display and light levels)
    /// into the bitstream, as FFmpeg's AMF encoder does from a frame's side
    /// data. Clients that size HDR from the stream rather than from the
    /// control channel's copy otherwise get no mastering metadata at all.
    pub fn set_hdr_metadata(&mut self, metadata: butterpollo_core::hdr::Metadata) {
        if !self.config.hdr || self.codec == 0 || self.hdr_metadata == Some(metadata) {
            return;
        }
        // Not retried every second when the driver refuses it.
        self.hdr_metadata = Some(metadata);
        match self.write_hdr_metadata(&metadata) {
            Ok(()) => tracing::info!(
                maximum_nits = metadata.maximum_nits,
                minimum = metadata.minimum,
                max_cll = metadata.max_cll,
                max_fall = metadata.max_fall,
                "AMF HDR metadata in the bitstream"
            ),
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "AMF HDR metadata not written")
            }
        }
    }
    fn write_hdr_metadata(&mut self, metadata: &butterpollo_core::hdr::Metadata) -> Result<()> {
        unsafe {
            let mut buffer = ptr::null_mut();
            check(((*(*self.context).pVtbl).AllocBuffer.unwrap())(
                self.context,
                AMF_MEMORY_TYPE_AMF_MEMORY_HOST,
                std::mem::size_of::<AMFHDRMetadata>(),
                &mut buffer,
            ))
            .context("AMF HDR metadata buffer")?;
            if buffer.is_null() {
                bail!("AMF returned no HDR metadata buffer");
            }
            // HEVC takes primaries and white in 1/50000 and luminance in
            // 1/10000 nit, as AMF documents. For AV1, AMF copies the values
            // into the metadata OBU unscaled, so they must already be in
            // AV1's units: 0.16 chromaticity, 24.8 and 18.14 fixed-point
            // luminance (the documented units read back as 39,062 nits).
            let av1 = self.codec == 2;
            let chroma = |[x, y]: [u16; 2]| {
                if av1 {
                    [x, y].map(|v| (u32::from(v) * 65536 / 50000).min(65535) as u16)
                } else {
                    [x, y]
                }
            };
            let (maximum, minimum) = if av1 {
                (
                    u32::from(metadata.maximum_nits) * 256,
                    u32::from(metadata.minimum) * 16384 / 10000,
                )
            } else {
                (
                    u32::from(metadata.maximum_nits) * 10000,
                    u32::from(metadata.minimum),
                )
            };
            let [red, green, blue] = metadata.primaries;
            ((*(*buffer).pVtbl).GetNative.unwrap())(buffer)
                .cast::<AMFHDRMetadata>()
                .write(AMFHDRMetadata {
                    redPrimary: chroma(red),
                    greenPrimary: chroma(green),
                    bluePrimary: chroma(blue),
                    whitePoint: chroma(metadata.white),
                    maxMasteringLuminance: maximum,
                    minMasteringLuminance: minimum,
                    maxContentLightLevel: metadata.max_cll,
                    maxFrameAverageLightLevel: metadata.max_fall,
                });
            // SetProperty keeps its own reference to the buffer.
            let result = self.property(
                "InHDRMetadata",
                AMFVariantStruct {
                    type_: AMF_VARIANT_TYPE_AMF_VARIANT_INTERFACE,
                    __bindgen_anon_1: AMFVariantStruct__bindgen_ty_1 {
                        pInterface: buffer.cast(),
                    },
                },
            );
            ((*(*buffer).pVtbl).Release.unwrap())(buffer);
            result
        }
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
    pub fn set_next_frame(&mut self, frame: u64) {
        debug_assert!(self.in_flight.is_empty());
        self.index = frame.saturating_sub(1).min(i64::MAX as u64) as i64;
    }
    pub fn supports_invalidation(&self) -> bool {
        self.references.enabled()
    }
    pub fn invalidate_ref_frames(&mut self, first: u64, last: u64) -> bool {
        // VCN AVC uses a four-bit frame_num with POC type 2. A decoder cannot
        // reliably infer a missing counter wrap. Use a recovery IDR for that
        // window, and keep LTR recovery for losses within the same epoch.
        if self.codec == 0 && first.saturating_sub(2) / 16 != self.index as u64 / 16 {
            return false;
        }
        self.references.invalidate(first, last)
    }
    fn ltr_properties(&self) -> [&'static str; 4] {
        match self.codec {
            0 => [
                "MaxOfLTRFrames",
                "LTRMode",
                "MarkCurrentWithLTRIndex",
                "ForceLTRReferenceBitfield",
            ],
            1 => [
                "HevcMaxOfLTRFrames",
                "HevcLTRMode",
                "HevcMarkCurrentWithLTRIndex",
                "HevcForceLTRReferenceBitfield",
            ],
            _ => [
                "Av1MaxNumLTRFrames",
                "Av1LTRMode",
                "Av1MarkCurrentWithLTRIndex",
                "Av1ForceLTRReferenceBitfield",
            ],
        }
    }
    fn configure_ltr(&mut self, mut count: usize) {
        if count == 0 || self.config.intra_refresh {
            return;
        }
        let [maximum, mode, _, _] = self.ltr_properties();
        let result = (|| -> Result<usize> {
            unsafe {
                let mut info = ptr::null();
                if ((*(*self.component).pVtbl).GetPropertyInfo.unwrap())(
                    self.component,
                    wide(maximum).as_ptr(),
                    &mut info,
                ) == AMF_RESULT_AMF_OK
                    && !info.is_null()
                {
                    let maximum = &(*info).maxValue;
                    if maximum.type_ == AMF_VARIANT_TYPE_AMF_VARIANT_INT64 {
                        count = count.min(maximum.__bindgen_anon_1.int64Value.max(0) as usize);
                    }
                }
                if self.codec == 2 {
                    let mut caps = ptr::null_mut();
                    if ((*(*self.component).pVtbl).GetCaps.unwrap())(self.component, &mut caps)
                        == AMF_RESULT_AMF_OK
                        && !caps.is_null()
                    {
                        let mut limit = int(0);
                        let result = ((*(*caps).pVtbl).GetProperty.unwrap())(
                            caps,
                            wide("Av1CapMaxNumLTRFrames").as_ptr(),
                            &mut limit,
                        );
                        if result == AMF_RESULT_AMF_OK
                            && limit.type_ == AMF_VARIANT_TYPE_AMF_VARIANT_INT64
                        {
                            count = count.min(limit.__bindgen_anon_1.int64Value.max(0) as usize);
                        }
                        ((*(*caps).pVtbl).Release.unwrap())(caps);
                    }
                }
                if count == 0 {
                    bail!("driver reports no LTR slots");
                }
                self.property_raw(maximum, int(count as i64))?;
                self.property_raw(mode, int(0))?;
                let mut applied = int(0);
                check(((*(*self.component).pVtbl).GetProperty.unwrap())(
                    self.component,
                    wide(maximum).as_ptr(),
                    &mut applied,
                ))?;
                if applied.type_ != AMF_VARIANT_TYPE_AMF_VARIANT_INT64
                    || applied.__bindgen_anon_1.int64Value <= 0
                {
                    bail!("LTR slot readback failed");
                }
                Ok(count.min(applied.__bindgen_anon_1.int64Value as usize))
            }
        })();
        match result {
            Ok(count) => {
                self.references = butterpollo_core::ltr::References::new(count);
                tracing::info!(count, "AMF long-term reference recovery enabled");
            }
            Err(error) => {
                let _ = self.property_raw(maximum, int(0));
                tracing::warn!(%error,"AMF LTR unavailable; using IDR recovery");
            }
        }
    }
    fn prepare_surface(
        &mut self,
        surface: *mut AMFSurface,
        mut idr: bool,
    ) -> Result<butterpollo_core::ltr::Plan> {
        let mut plan = self.references.plan(self.index as u64 + 1, idr);
        let [_, _, mark, reference] = self.ltr_properties();
        let apply = |name: &str, value: AMFVariantStruct| -> Result<()> {
            unsafe {
                check(((*(*surface).pVtbl).SetProperty.unwrap())(
                    surface,
                    wide(name).as_ptr(),
                    value,
                ))
            }
        };
        let ltr = (|| -> Result<()> {
            if let Some(slot) = plan.mark {
                apply(mark, int(slot as i64))?;
            }
            if let Some(slot) = plan.reference {
                apply(reference, int(1 << slot))?;
            }
            Ok(())
        })();
        if let Err(error) = ltr {
            tracing::warn!(%error,"AMF LTR surface rejected; requesting IDR recovery");
            self.references.disable();
            idr = true;
            plan = self.references.plan(self.index as u64 + 1, true);
        }
        if idr {
            apply(
                match self.codec {
                    0 => "ForcePictureType",
                    1 => "HevcForcePictureType",
                    _ => "Av1ForceFrameType",
                },
                int(if self.codec == 2 { 1 } else { 2 }),
            )?;
            let headers: &[&str] = match self.codec {
                0 => &["InsertSPS", "InsertPPS"],
                1 => &["HevcInsertHeader"],
                _ => &[],
            };
            for header in headers {
                apply(header, crate::amf_gpu::boolean(true))?;
            }
        }
        Ok(plan)
    }
    pub fn poll(&mut self) -> Result<Vec<Encoded>> {
        // A submission the driver never answers would otherwise keep every
        // later query waiting for its timeout.
        while self
            .in_flight
            .front()
            .is_some_and(|s| s.started.elapsed() > Duration::from_secs(2))
        {
            let stale = self.in_flight.pop_front();
            tracing::warn!(
                pts = stale.map(|s| s.pts),
                "AMF returned no output for a frame"
            );
        }
        unsafe {
            let mut output = vec![];
            // With a query timeout, asking again after the last frame in flight
            // has been returned would only wait for the timeout.
            while !self.in_flight.is_empty() {
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
                let submission = self
                    .in_flight
                    .iter()
                    .position(|s| s.pts == pts)
                    .and_then(|position| self.in_flight.remove(position));
                let latency = submission.as_ref().map(|s| s.started.elapsed());
                let presentation = submission.as_ref().map(|s| s.presentation);
                let after_invalidation = submission.is_some_and(|s| s.after_invalidation);
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
                let bytes = std::slice::from_raw_parts(raw, size as usize);
                let bytes = if self.codec == 0 && idr && self.references.enabled() {
                    butterpollo_core::bitstream::h264_reference_recovery(bytes)
                } else {
                    bytes.to_vec()
                };
                (v.Release.unwrap())(buffer);
                output.push(Encoded {
                    bytes,
                    idr,
                    after_invalidation,
                    latency,
                    presentation,
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
            self.convert = Some(Convert::new_config(
                &self.config,
                if self.config.ten_bit() {
                    ff::AVPixelFormat_AV_PIX_FMT_P010LE
                } else {
                    ff::AVPixelFormat_AV_PIX_FMT_NV12
                },
            )?);
        }
        let convert = self.convert.as_mut().unwrap();
        convert.luminance = self.luminance;
        convert.convert(image)?;
        let frame = convert.frame;
        unsafe {
            let mut surface = ptr::null_mut();
            check(((*(*self.context).pVtbl).AllocSurface.unwrap())(
                self.context,
                AMF_MEMORY_TYPE_AMF_MEMORY_HOST,
                if self.config.ten_bit() {
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
                check((v.SetCrop.unwrap())(
                    surface,
                    0,
                    0,
                    self.config.width as i32,
                    self.config.height as i32,
                ))
                .context("AMF host surface crop")?;
                (v.SetPts.unwrap())(surface, self.index);
                (v.SetDuration.unwrap())(
                    surface,
                    10_000_000_000 / i64::from(self.config.fps_millihz()),
                );
                for plane_index in 0..2 {
                    let plane = (v.GetPlaneAt.unwrap())(surface, plane_index);
                    if plane.is_null() {
                        bail!("AMF plane missing");
                    }
                    let pv = &*(*plane).pVtbl;
                    let dst = (pv.GetNative.unwrap())(plane) as *mut u8;
                    let pitch = (pv.GetHPitch.unwrap())(plane) as usize;
                    let width =
                        self.config.width as usize * if self.config.ten_bit() { 2 } else { 1 };
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
                Ok(())
            })();
            if let Err(e) = prepared {
                (v.Release.unwrap())(surface);
                return Err(e);
            }
            let plan = match self.prepare_surface(surface, idr) {
                Ok(plan) => plan,
                Err(error) => {
                    (v.Release.unwrap())(surface);
                    return Err(error);
                }
            };
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
                presentation: image.captured,
                after_invalidation: plan.after_invalidation,
                _capture: None,
                _converted: None,
            });
            self.references.accepted(self.index as u64 + 1, &plan);
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
            for suffix in ["PeakBitrate", "VBVBufferSize", "MaxAUSize"] {
                let prefix = match self.codec {
                    0 => "",
                    1 => "Hevc",
                    _ => "Av1",
                };
                let name = format!("{prefix}{suffix}");
                unsafe {
                    let mut current = int(0);
                    let mut info = ptr::null();
                    let table = &*(*self.component).pVtbl;
                    if (table.GetProperty.unwrap())(
                        self.component,
                        wide(&name).as_ptr(),
                        &mut current,
                    ) == AMF_RESULT_AMF_OK
                        && current.type_ == AMF_VARIANT_TYPE_AMF_VARIANT_INT64
                        && current.__bindgen_anon_1.int64Value > 0
                        && (table.GetPropertyInfo.unwrap())(
                            self.component,
                            wide(&name).as_ptr(),
                            &mut info,
                        ) == AMF_RESULT_AMF_OK
                        && !info.is_null()
                    {
                        let mut scaled = current
                            .__bindgen_anon_1
                            .int64Value
                            .saturating_mul(i64::from(bitrate))
                            / i64::from(self.bitrate.max(1));
                        if (*info).minValue.type_ == AMF_VARIANT_TYPE_AMF_VARIANT_INT64 {
                            scaled = scaled.max((*info).minValue.__bindgen_anon_1.int64Value);
                        }
                        if (*info).maxValue.type_ == AMF_VARIANT_TYPE_AMF_VARIANT_INT64 {
                            scaled = scaled.min((*info).maxValue.__bindgen_anon_1.int64Value);
                        }
                        if let Err(error) = self.property_raw(&name, int(scaled)) {
                            tracing::debug!(%error,"AMF driver retains its existing rate-control buffer");
                        }
                    }
                }
            }
            self.bitrate = bitrate;
        }
        Ok(())
    }
    pub fn accepts_gpu_device(&self, image: &GpuImage) -> bool {
        self._device.device.as_raw() == image.gpu.device.as_raw()
            && (self.compute.is_none() || crate::compute::shareable(&image.texture))
    }
    fn wait_capacity(&mut self) -> Result<Vec<Encoded>> {
        let mut output = self.poll()?;
        let deadline = Instant::now() + Duration::from_millis(100);
        while self.in_flight.len() >= 8 || self.ownership.retained() >= 8 {
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
        let (surface, converted) = if let Some(input) = self.compute.as_mut() {
            let compute = input.converter.compute().clone();
            let texture = compute.open(&image.texture)?;
            let pointer = image
                .cursor
                .as_ref()
                .map(|cursor| input.converter.pointer(cursor))
                .transpose()?;
            input.converter.values = crate::gpu_color::constants(
                &self.config,
                source,
                self.luminance,
                image.cursor.as_ref(),
            );
            let converted = input.converter.convert(
                &texture,
                crate::compute::format(image.pixel),
                pointer.as_ref(),
                image.ready.as_ref(),
            )?;
            crate::amf_gpu::synchronize(&converted.texture, &converted.fence, converted.value)?;
            let surface = input
                .context
                .wrap(&converted.texture, &mut self.ownership)?;
            (surface, None)
        } else {
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
            self.gpu_convert
                .as_mut()
                .unwrap()
                .color
                .set_luminance(self.luminance);
            let (surface, texture) = self.gpu_convert.as_mut().unwrap().convert(
                self.context,
                image,
                &mut self.ownership,
            )?;
            (surface, Some(texture))
        };
        self.set_bitrate(bitrate)?;
        unsafe {
            let v = &*(*surface.0).pVtbl;
            check((v.SetCrop.unwrap())(
                surface.0,
                0,
                0,
                self.config.width as i32,
                self.config.height as i32,
            ))
            .context("AMF native surface crop")?;
            (v.SetPts.unwrap())(surface.0, self.index);
            (v.SetDuration.unwrap())(
                surface.0,
                10_000_000_000 / i64::from(self.config.fps_millihz()),
            );
            let plan = self.prepare_surface(surface.0, idr)?;
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
                presentation: image.captured,
                after_invalidation: plan.after_invalidation,
                _capture: Some(image.clone()),
                _converted: converted,
            });
            self.references.accepted(self.index as u64 + 1, &plan);
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
            // After the encoder has let go of its D3D12 inputs.
            self.compute.take();
            if !self.context.is_null() {
                ((*(*self.context).pVtbl).Terminate.unwrap())(self.context);
                ((*(*self.context).pVtbl).Release.unwrap())(self.context);
            }
        }
    }
}

#[cfg(test)]
mod tests;
