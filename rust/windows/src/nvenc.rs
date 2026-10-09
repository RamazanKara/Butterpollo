//! Native NVIDIA NVENC encoder, called through NVIDIA's driver ABI.
//!
//! Supports NVENC API 11 to 13 and reference frame invalidation. GPU
//! resources stay owned until their output completes; 10-bit 4:4:4 input goes
//! through CUDA interop (`cuda`).

mod cuda;

use crate::{
    capture::{Device, GpuImage, Image, Pixel},
    encoder::Encoded,
    nvenc_abi::*,
};
use anyhow::{Context, Result, bail};
use butterpollo_core::{
    config::Config,
    nvenc::{ApiVersion, Recovery, References, Tuning},
    rtsp::Negotiated,
};
use std::{
    collections::VecDeque,
    ffi::{CStr, c_void},
    ptr,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Graphics::Direct3D11::ID3D11Texture2D,
        System::{
            LibraryLoader::LOAD_LIBRARY_SEARCH_SYSTEM32,
            SystemInformation::GetSystemDirectoryW,
            Threading::{CreateEventW, WaitForSingleObject},
        },
    },
    core::Interface,
};

const SUCCESS: NVENCSTATUS = _NVENCSTATUS_NV_ENC_SUCCESS;
const MAX_PENDING: usize = 8;
const COMPLETION_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
type CreateInstance = unsafe extern "C" fn(*mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS;
type GetMaximum = unsafe extern "C" fn(*mut u32) -> NVENCSTATUS;

pub(crate) fn system_library(name: &str) -> Result<libloading::Library> {
    let mut directory = [0u16; 32768];
    // SAFETY: `directory` is a live, writable UTF-16 buffer and the call writes at most its length.
    let length = unsafe { GetSystemDirectoryW(Some(&mut directory)) } as usize;
    if length == 0 || length >= directory.len() {
        bail!("cannot locate the Windows system directory");
    }
    let path = std::path::PathBuf::from(String::from_utf16(&directory[..length])?).join(name);
    // SAFETY: `path` is an absolute System32 path, so no search-order DLL is picked up; loading
    // runs only the installed driver's initialisers.
    Ok(unsafe {
        libloading::os::windows::Library::load_with_flags(path, LOAD_LIBRARY_SEARCH_SYSTEM32.0)?
    }
    .into())
}
struct Runtime {
    _library: Option<libloading::Library>,
    create: CreateInstance,
    maximum: ApiVersion,
}
impl Runtime {
    fn load() -> Result<Arc<Self>> {
        // SAFETY: `library` moves into the Runtime, keeping the copied `create` pointer loaded;
        // both symbols use NVIDIA's documented C signatures.
        unsafe {
            let library =
                system_library("nvEncodeAPI64.dll").context("NVIDIA encode driver unavailable")?;
            let create = *library.get::<CreateInstance>(b"NvEncodeAPICreateInstance\0")?;
            let maximum = if let Ok(get) =
                library.get::<GetMaximum>(b"NvEncodeAPIGetMaxSupportedVersion\0")
            {
                let mut value = 0;
                check_raw(
                    get(&mut value),
                    "query maximum API",
                    &NV_ENCODE_API_FUNCTION_LIST::default(),
                    ptr::null_mut(),
                )?;
                ApiVersion::from_driver(value)
            } else {
                ApiVersion::COMPILED
            };
            Ok(Arc::new(Self {
                _library: Some(library),
                create,
                maximum,
            }))
        }
    }
}
#[derive(Debug)]
struct DriverError {
    status: NVENCSTATUS,
    operation: &'static str,
    detail: String,
}
impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "NVENC {} failed ({}): {}",
            self.operation, self.status, self.detail
        )
    }
}
impl std::error::Error for DriverError {}
fn check_raw(
    status: NVENCSTATUS,
    operation: &'static str,
    functions: &NV_ENCODE_API_FUNCTION_LIST,
    raw: *mut c_void,
) -> Result<()> {
    if status == SUCCESS {
        return Ok(());
    }
    let detail = if !raw.is_null() {
        functions
            .nvEncGetLastErrorString
            .and_then(|get| {
                // SAFETY: `raw` is non-null and every caller passes the live session that
                // `functions` was created for.
                let message = unsafe { get(raw) };
                (!message.is_null()).then(|| {
                    // SAFETY: The driver returns a NUL-terminated string that stays valid until the
                    // next call on `raw`.
                    unsafe { CStr::from_ptr(message) }
                        .to_string_lossy()
                        .into_owned()
                })
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    Err(DriverError {
        status,
        operation,
        detail,
    }
    .into())
}
fn retry_version(error: &anyhow::Error) -> bool {
    error.downcast_ref::<DriverError>().is_some_and(|e| {
        matches!(
            e.status,
            _NVENCSTATUS_NV_ENC_ERR_INVALID_VERSION | _NVENCSTATUS_NV_ENC_ERR_UNSUPPORTED_PARAM
        )
    })
}
fn validate(functions: &NV_ENCODE_API_FUNCTION_LIST) -> Result<()> {
    macro_rules! require { ($($entry:ident),+ $(,)?) => { $(
        if functions.$entry.is_none() { bail!(concat!("NVENC driver lacks ", stringify!($entry))); }
    )+ }; }
    require!(
        nvEncOpenEncodeSessionEx,
        nvEncGetEncodeCaps,
        nvEncInitializeEncoder,
        nvEncRegisterResource,
        nvEncUnregisterResource,
        nvEncMapInputResource,
        nvEncUnmapInputResource,
        nvEncCreateBitstreamBuffer,
        nvEncDestroyBitstreamBuffer,
        nvEncEncodePicture,
        nvEncLockBitstream,
        nvEncUnlockBitstream,
        nvEncDestroyEncoder
    );
    Ok(())
}
fn codec_guid(codec: u8) -> Result<GUID> {
    match codec {
        0 => Ok(NV_ENC_CODEC_H264_GUID),
        1 => Ok(NV_ENC_CODEC_HEVC_GUID),
        2 => Ok(NV_ENC_CODEC_AV1_GUID),
        _ => bail!("unsupported NVENC codec"),
    }
}
fn preset_guid(preset: usize) -> GUID {
    [
        NV_ENC_PRESET_P1_GUID,
        NV_ENC_PRESET_P2_GUID,
        NV_ENC_PRESET_P3_GUID,
        NV_ENC_PRESET_P4_GUID,
        NV_ENC_PRESET_P5_GUID,
        NV_ENC_PRESET_P6_GUID,
        NV_ENC_PRESET_P7_GUID,
    ][preset.clamp(1, 7) - 1]
}
fn buffer_format(config: &Negotiated) -> NV_ENC_BUFFER_FORMAT {
    match (config.yuv444, config.ten_bit()) {
        (false, false) => _NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_NV12,
        (false, true) => _NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_YUV420_10BIT,
        (true, false) => _NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_AYUV,
        (true, true) => _NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_YUV444_10BIT,
    }
}
fn vui(config: &Negotiated) -> NV_ENC_CONFIG_H264_VUI_PARAMETERS {
    let (primaries, transfer, matrix) = colors(config);
    NV_ENC_CONFIG_H264_VUI_PARAMETERS {
        videoSignalTypePresentFlag: 1,
        videoFormat: 5,
        videoFullRangeFlag: u32::from(config.full_range()),
        colourDescriptionPresentFlag: 1,
        colourPrimaries: primaries,
        transferCharacteristics: transfer,
        colourMatrix: matrix,
        chromaSampleLocationFlag: u32::from(!config.yuv444),
        bitstreamRestrictionFlag: 1,
        ..Default::default()
    }
}
fn colors(config: &Negotiated) -> (u32, u32, u32) {
    let (primaries, matrix, transfer) = match config.color_matrix() {
        0 => (6, 6, 6),
        2 => (9, 9, 14),
        _ => (1, 1, 1),
    };
    (primaries, if config.hdr { 16 } else { transfer }, matrix)
}
struct Metadata {
    mastering: Box<MASTERING_DISPLAY_INFO>,
    light: Box<CONTENT_LIGHT_LEVEL>,
}
impl Metadata {
    fn update(&mut self, metadata: butterpollo_core::hdr::Metadata) {
        self.mastering.r = CHROMA_POINTS {
            x: metadata.primaries[0][0],
            y: metadata.primaries[0][1],
        };
        self.mastering.g = CHROMA_POINTS {
            x: metadata.primaries[1][0],
            y: metadata.primaries[1][1],
        };
        self.mastering.b = CHROMA_POINTS {
            x: metadata.primaries[2][0],
            y: metadata.primaries[2][1],
        };
        self.mastering.whitePoint = CHROMA_POINTS {
            x: metadata.white[0],
            y: metadata.white[1],
        };
        self.mastering.maxLuma = u32::from(metadata.maximum_nits) * 10000;
        self.mastering.minLuma = u32::from(metadata.minimum);
        self.light.maxContentLightLevel = metadata.max_cll;
        self.light.maxPicAverageLightLevel = metadata.max_fall;
    }
}
impl Default for Metadata {
    fn default() -> Self {
        // Same mastering defaults as the Moonlight HDR control message.
        Self {
            mastering: Box::new(MASTERING_DISPLAY_INFO {
                r: CHROMA_POINTS { x: 35400, y: 14600 },
                g: CHROMA_POINTS { x: 8500, y: 39850 },
                b: CHROMA_POINTS { x: 6550, y: 2300 },
                whitePoint: CHROMA_POINTS { x: 15635, y: 16450 },
                maxLuma: 1000 * 10000,
                minLuma: 1,
            }),
            light: Box::default(),
        }
    }
}
struct Input {
    key: usize,
    raw: *mut c_void,
    kind: NV_ENC_INPUT_RESOURCE_TYPE,
    pitch: u32,
    _texture: Option<ID3D11Texture2D>,
    cuda: Option<cuda::Input>,
}
struct Slot {
    input: Input,
    metadata: Metadata,
    registered: NV_ENC_REGISTERED_PTR,
    mapped: NV_ENC_INPUT_PTR,
    output: NV_ENC_OUTPUT_PTR,
    event: HANDLE,
    event_registered: bool,
    signaled: bool,
}
impl Drop for Slot {
    fn drop(&mut self) {
        if !self.event.is_invalid() {
            // SAFETY: This slot alone owns the event that `Session::slot` created.
            let _ = unsafe { CloseHandle(self.event) };
        }
    }
}
struct Pending {
    slot: usize,
    frame: u64,
    started: Instant,
    presentation: Instant,
    after_invalidation: bool,
    _converted: Option<Arc<ID3D11Texture2D>>,
}
struct Session {
    raw: *mut c_void,
    functions: NV_ENCODE_API_FUNCTION_LIST,
    api: ApiVersion,
    _runtime: Arc<Runtime>,
    cuda: Option<Rc<cuda::Context>>,
    config: Box<NV_ENC_CONFIG>,
    initialize: Box<NV_ENC_INITIALIZE_PARAMS>,
    stream: Negotiated,
    metadata: Metadata,
    slots: Vec<Slot>,
    pending: VecDeque<Pending>,
    ready: VecDeque<Encoded>,
    references: References,
    retained: u32,
    invalidation: bool,
    dynamic_bitrate: bool,
    next: u64,
    initialized: bool,
    force_idr: bool,
}
impl Session {
    fn open(
        runtime: Arc<Runtime>,
        device: *mut c_void,
        kind: NV_ENC_DEVICE_TYPE,
        cuda: Option<Rc<cuda::Context>>,
        stream: &Negotiated,
        tuning: &Tuning,
    ) -> Result<Self> {
        let _guard = cuda.as_ref().map(|ctx| ctx.enter()).transpose()?;
        let mut failures = Vec::new();
        for api in ApiVersion::candidates(stream.codec, runtime.maximum) {
            match Self::open_version(
                runtime.clone(),
                device,
                kind,
                cuda.clone(),
                stream,
                tuning,
                api,
            ) {
                Ok(session) => return Ok(session),
                Err(error) if retry_version(&error) => {
                    tracing::warn!(error = %format!("{error:#}"), major = api.0, minor = api.1, "NVENC rejected this driver API; retrying a reviewed older API. Update the NVIDIA driver, or use Vibepollo if encoding remains unstable");
                    failures.push(format!("{}.{}: {error}", api.0, api.1))
                }
                Err(error) => return Err(error),
            }
        }
        bail!(
            "no reviewed NVENC driver API supports this stream: {}",
            failures.join("; ")
        )
    }
    fn open_version(
        runtime: Arc<Runtime>,
        device: *mut c_void,
        kind: NV_ENC_DEVICE_TYPE,
        cuda: Option<Rc<cuda::Context>>,
        stream: &Negotiated,
        tuning: &Tuning,
        api: ApiVersion,
    ) -> Result<Self> {
        let mut functions = NV_ENCODE_API_FUNCTION_LIST {
            version: api.structure(2, false),
            ..Default::default()
        };
        check_raw(
            // SAFETY: `create` is NvEncodeAPICreateInstance from the still-loaded DLL and
            // `functions` is versioned.
            unsafe { (runtime.create)(&mut functions) },
            "create function table",
            &functions,
            ptr::null_mut(),
        )?;
        validate(&functions)?;
        let mut session = Self {
            raw: ptr::null_mut(),
            functions,
            api,
            _runtime: runtime,
            cuda,
            config: Box::default(),
            initialize: Box::default(),
            stream: stream.clone(),
            metadata: Metadata::default(),
            slots: Vec::new(),
            pending: VecDeque::new(),
            ready: VecDeque::new(),
            references: References::default(),
            retained: 1,
            invalidation: false,
            dynamic_bitrate: false,
            next: 1,
            initialized: false,
            force_idr: false,
        };
        let mut open = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
            version: api.structure(1, false),
            deviceType: kind,
            device,
            apiVersion: api.packed(),
            ..Default::default()
        };
        // The owner exists before the driver call, so even a partially opened
        // rejected version is destroyed before the next candidate is tried.
        // SAFETY: `validate` confirmed nvEncOpenEncodeSessionEx exists, `device` is the caller's
        // live D3D11 device or CUDA context, and both out-pointers are live locals.
        let status =
            unsafe { (functions.nvEncOpenEncodeSessionEx.unwrap())(&mut open, &mut session.raw) };
        session.check(status, "open session")?;
        if session.raw.is_null() {
            bail!("NVENC returned no session");
        }
        session.configure(tuning)?;
        Ok(session)
    }
    fn guard(&self) -> Result<Option<cuda::Guard>> {
        self.cuda.as_ref().map(|ctx| ctx.enter()).transpose()
    }
    fn check(&self, status: NVENCSTATUS, operation: &'static str) -> Result<()> {
        check_raw(status, operation, &self.functions, self.raw)
    }
    fn cap(&self, cap: NV_ENC_CAPS) -> Result<i32> {
        let mut parameters = NV_ENC_CAPS_PARAM {
            version: self.api.structure(1, false),
            capsToQuery: cap,
            ..Default::default()
        };
        let mut value = 0;
        self.check(
            // SAFETY: `self.raw` is the open session, `validate` checked nvEncGetEncodeCaps, and
            // outputs are locals.
            unsafe {
                (self.functions.nvEncGetEncodeCaps.unwrap())(
                    self.raw,
                    codec_guid(self.stream.codec)?,
                    &mut parameters,
                    &mut value,
                )
            },
            "query capability",
        )?;
        Ok(value)
    }
    fn optional_cap(&self, cap: NV_ENC_CAPS) -> bool {
        self.cap(cap).unwrap_or(0) > 0
    }
    fn configure(&mut self, tuning: &Tuning) -> Result<()> {
        let _guard = self.guard()?;
        self.stream.validate()?;
        if self.stream.width > self.cap(_NV_ENC_CAPS_NV_ENC_CAPS_WIDTH_MAX)?.max(0) as u32
            || self.stream.height > self.cap(_NV_ENC_CAPS_NV_ENC_CAPS_HEIGHT_MAX)?.max(0) as u32
        {
            bail!("stream dimensions exceed NVENC capability");
        }
        if self.stream.ten_bit()
            && (!self.optional_cap(_NV_ENC_CAPS_NV_ENC_CAPS_SUPPORT_10BIT_ENCODE)
                || (self.stream.codec == 0 && !self.api.modern_depth()))
        {
            bail!("this NVENC driver/codec cannot encode 10-bit input");
        }
        if self.stream.yuv444 && !self.optional_cap(_NV_ENC_CAPS_NV_ENC_CAPS_SUPPORT_YUV444_ENCODE)
        {
            bail!("this NVENC codec does not support 4:4:4");
        }
        let asynchronous = self.optional_cap(_NV_ENC_CAPS_NV_ENC_CAPS_ASYNC_ENCODE_SUPPORT)
            && self.functions.nvEncRegisterAsyncEvent.is_some()
            && self.functions.nvEncUnregisterAsyncEvent.is_some();
        self.retained = if self.optional_cap(_NV_ENC_CAPS_NV_ENC_CAPS_SUPPORT_MULTIPLE_REF_FRAMES) {
            if self.stream.references > 0 {
                self.stream.references
            } else if self.stream.codec == 2 {
                8
            } else {
                5
            }
        } else {
            1
        };
        self.invalidation = self.retained > 1
            && self.functions.nvEncInvalidateRefFrames.is_some()
            && self.optional_cap(_NV_ENC_CAPS_NV_ENC_CAPS_SUPPORT_REF_PIC_INVALIDATION);
        self.dynamic_bitrate = self.functions.nvEncReconfigureEncoder.is_some()
            && self.optional_cap(_NV_ENC_CAPS_NV_ENC_CAPS_SUPPORT_DYN_BITRATE_CHANGE);
        let mut preset = NV_ENC_PRESET_CONFIG {
            version: self.api.preset(),
            presetCfg: NV_ENC_CONFIG {
                version: self.api.config(),
                ..Default::default()
            },
            ..Default::default()
        };
        let guid = codec_guid(self.stream.codec)?;
        let preset_guid = preset_guid(tuning.preset);
        let result = if let Some(get) = self.functions.nvEncGetEncodePresetConfigEx {
            // SAFETY: `get` is the driver's own preset entry, `self.raw` is the open session and
            // `preset` is versioned.
            unsafe {
                get(
                    self.raw,
                    guid,
                    preset_guid,
                    NV_ENC_TUNING_INFO_NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
                    &mut preset,
                )
            }
        } else {
            _NVENCSTATUS_NV_ENC_ERR_UNIMPLEMENTED
        };
        if result != SUCCESS {
            if let Some(get) = self.functions.nvEncGetEncodePresetConfig {
                tracing::warn!(
                    code = result,
                    "NVENC low-latency preset query failed; using a compatible preset with explicit low-latency settings. Update the NVIDIA driver or use Vibepollo if frame times worsen"
                );
                preset = NV_ENC_PRESET_CONFIG {
                    version: self.api.preset(),
                    presetCfg: NV_ENC_CONFIG {
                        version: self.api.config(),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                self.check(
                    // SAFETY: `get` is the driver's legacy preset entry for the open session and
                    // `preset` is versioned.
                    unsafe { get(self.raw, guid, preset_guid, &mut preset) },
                    "query compatible preset",
                )?;
            } else {
                self.check(result, "query low latency preset")?;
            }
        }
        *self.config = preset.presetCfg;
        self.config.version = self.api.config();
        self.config.gopLength = u32::MAX;
        self.config.frameIntervalP = 1;
        self.config.frameFieldMode =
            _NV_ENC_PARAMS_FRAME_FIELD_MODE_NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME;
        let custom_vbv = self.optional_cap(_NV_ENC_CAPS_NV_ENC_CAPS_SUPPORT_CUSTOM_VBV_BUF_SIZE);
        let temporal_aq =
            tuning.temporal_aq && self.optional_cap(_NV_ENC_CAPS_NV_ENC_CAPS_SUPPORT_TEMPORAL_AQ);
        if !custom_vbv {
            tracing::warn!(
                "NVENC custom VBV unavailable; the driver controls buffering and may increase latency. Update the NVIDIA driver or use Vibepollo"
            );
        }
        if tuning.temporal_aq && !temporal_aq {
            tracing::warn!(
                "NVENC temporal AQ unavailable; encoding without the requested quality feature. Update the NVIDIA driver or disable temporal AQ"
            );
        }
        let rc = &mut self.config.rcParams;
        rc.version = self.api.structure(1, false);
        rc.rateControlMode = _NV_ENC_PARAMS_RC_MODE_NV_ENC_PARAMS_RC_CBR;
        rc.averageBitRate = self.stream.bitrate_kbps * 1000;
        rc.maxBitRate = rc.averageBitRate;
        rc.set_zeroReorderDelay(1);
        rc.set_enableLookahead(0);
        rc.lookaheadDepth = 0;
        rc.lowDelayKeyFrameScale = 1;
        rc.multiPass = tuning.multipass;
        rc.set_enableAQ(u32::from(tuning.spatial_aq));
        rc.set_enableTemporalAQ(u32::from(temporal_aq));
        if custom_vbv {
            rc.vbvBufferSize = tuning.vbv(self.stream.bitrate_kbps, self.stream.fps_millihz());
        }
        if let Some(qp) = tuning.min_qp {
            rc.set_enableMinQP(1);
            rc.minQP = NV_ENC_QP {
                qpInterP: qp,
                qpInterB: qp,
                qpIntra: qp,
            };
        }
        let intra_refresh = self.stream.intra_refresh
            && self.optional_cap(_NV_ENC_CAPS_NV_ENC_CAPS_SUPPORT_INTRA_REFRESH);
        let single_slice =
            intra_refresh && self.optional_cap(_NV_ENC_CAPS_NV_ENC_CAPS_SINGLE_SLICE_INTRA_REFRESH);
        if self.stream.intra_refresh && !intra_refresh {
            tracing::warn!(
                "NVENC intra-refresh unavailable; keeping IDR recovery, which can cause larger recovery frames. Reduce bitrate on lossy links or use Vibepollo"
            );
        }
        let ten = self.stream.ten_bit();
        let depth = if ten { 10 } else { 8 };
        let chroma = if self.stream.yuv444 { 3 } else { 1 };
        let metadata = self.stream.hdr && self.api >= ApiVersion(13, 0);
        let cabac = !tuning.cavlc && self.optional_cap(_NV_ENC_CAPS_NV_ENC_CAPS_SUPPORT_CABAC);
        if self.stream.hdr && !metadata {
            tracing::warn!(
                "NVENC driver API cannot embed HDR mastering metadata; only control-channel metadata is available. Update the NVIDIA driver or use Vibepollo if HDR tone mapping is wrong"
            );
        }
        if self.stream.codec == 0 && !tuning.cavlc && !cabac {
            tracing::warn!(
                "NVENC CABAC unavailable; using CAVLC with lower compression efficiency. Update the NVIDIA driver or select HEVC or AV1"
            );
        }
        match self.stream.codec {
            0 => {
                self.config.profileGUID = if self.stream.yuv444 {
                    NV_ENC_H264_PROFILE_HIGH_444_GUID
                } else if ten {
                    NV_ENC_H264_PROFILE_HIGH_10_GUID
                } else {
                    NV_ENC_H264_PROFILE_HIGH_GUID
                };
                // SAFETY: The preset was queried for the H.264 GUID, so this union member is
                // active; it holds only integers and raw pointers.
                let codec = unsafe { &mut self.config.encodeCodecConfig.h264Config };
                codec.set_repeatSPSPPS(1);
                codec.idrPeriod = u32::MAX;
                codec.sliceMode = 3;
                codec.sliceModeData = self.stream.slices.max(1);
                codec.chromaFormatIDC = chroma;
                codec.set_enableFillerDataInsertion(u32::from(tuning.filler));
                codec.entropyCodingMode = if cabac {
                    _NV_ENC_H264_ENTROPY_CODING_MODE_NV_ENC_H264_ENTROPY_CODING_MODE_CABAC
                } else {
                    _NV_ENC_H264_ENTROPY_CODING_MODE_NV_ENC_H264_ENTROPY_CODING_MODE_CAVLC
                };
                codec.maxNumRefFrames = self.retained;
                codec.numRefL0 = 1;
                if self.api.modern_depth() {
                    codec.inputBitDepth = depth;
                    codec.outputBitDepth = depth;
                }
                codec.h264VUIParameters = vui(&self.stream);
                codec.set_enableIntraRefresh(u32::from(intra_refresh));
                codec.set_singleSliceIntraRefresh(u32::from(single_slice));
                if intra_refresh {
                    codec.intraRefreshPeriod = 300;
                    codec.intraRefreshCnt = 299;
                    codec.set_outputRecoveryPointSEI(1);
                }
            }
            1 => {
                self.config.profileGUID = if self.stream.yuv444 {
                    NV_ENC_HEVC_PROFILE_FREXT_GUID
                } else if ten {
                    NV_ENC_HEVC_PROFILE_MAIN10_GUID
                } else {
                    NV_ENC_HEVC_PROFILE_MAIN_GUID
                };
                // SAFETY: The preset was queried for the HEVC GUID, so this union member is active;
                // it holds only integers and raw pointers.
                let codec = unsafe { &mut self.config.encodeCodecConfig.hevcConfig };
                codec.set_repeatSPSPPS(1);
                codec.idrPeriod = u32::MAX;
                codec.sliceMode = 3;
                codec.sliceModeData = self.stream.slices.max(1);
                codec.set_chromaFormatIDC(chroma);
                codec.set_enableFillerDataInsertion(u32::from(tuning.filler));
                codec.maxNumRefFramesInDPB = self.retained;
                codec.numRefL0 = 1;
                if self.api.modern_depth() {
                    codec.inputBitDepth = depth;
                    codec.outputBitDepth = depth;
                } else if ten {
                    codec.set_reserved3(2);
                }
                codec.hevcVUIParameters = vui(&self.stream);
                codec.set_enableIntraRefresh(u32::from(intra_refresh));
                codec.set_singleSliceIntraRefresh(u32::from(single_slice));
                if intra_refresh {
                    codec.intraRefreshPeriod = 300;
                    codec.intraRefreshCnt = 299;
                    codec.set_outputRecoveryPointSEI(1);
                }
                if metadata {
                    codec.set_outputMasteringDisplay(1);
                }
            }
            2 => {
                self.config.profileGUID = NV_ENC_AV1_PROFILE_MAIN_GUID;
                // SAFETY: The preset was queried for the AV1 GUID, so this union member is active;
                // it holds only integers and raw pointers.
                let codec = unsafe { &mut self.config.encodeCodecConfig.av1Config };
                codec.set_repeatSeqHdr(1);
                codec.idrPeriod = u32::MAX;
                codec.set_chromaFormatIDC(chroma);
                codec.set_enableBitstreamPadding(u32::from(tuning.filler));
                codec.maxNumRefFramesInDPB = self.retained;
                codec.numFwdRefs = 1;
                if self.api.modern_depth() {
                    codec.inputBitDepth = depth;
                    codec.outputBitDepth = depth;
                } else if ten {
                    codec.set_enableTemporalSVC(1);
                    codec.set_reserved4(1);
                }
                let (primaries, transfer, matrix) = colors(&self.stream);
                codec.colorPrimaries = primaries;
                codec.transferCharacteristics = transfer;
                codec.matrixCoefficients = matrix;
                codec.colorRange = u32::from(self.stream.full_range());
                codec.chromaSamplePosition = u32::from(!self.stream.yuv444);
                if self.stream.slices > 1 {
                    let power = (self.stream.slices as f64).log2();
                    codec.numTileRows = 1 << (power / 2.).ceil() as u32;
                    codec.numTileColumns = 1 << (power / 2.).floor() as u32;
                }
                if metadata {
                    codec.set_outputMasteringDisplay(1);
                }
            }
            _ => unreachable!(),
        }
        *self.initialize = NV_ENC_INITIALIZE_PARAMS {
            version: self.api.initialize(),
            encodeGUID: guid,
            presetGUID: preset_guid,
            encodeWidth: self.stream.width,
            encodeHeight: self.stream.height,
            darWidth: self.stream.width,
            darHeight: self.stream.height,
            frameRateNum: self.stream.fps_millihz(),
            frameRateDen: 1000,
            enableEncodeAsync: u32::from(asynchronous),
            enablePTD: 1,
            tuningInfo: NV_ENC_TUNING_INFO_NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
            encodeConfig: self.config.as_mut(),
            ..Default::default()
        };
        let weighted = tuning.weighted_prediction
            && self.optional_cap(_NV_ENC_CAPS_NV_ENC_CAPS_SUPPORT_WEIGHTED_PREDICTION);
        self.initialize
            .set_enableWeightedPrediction(u32::from(weighted));
        if self.stream.codec != 0
            && self.api >= ApiVersion(12, 1)
            && !(self.stream.codec == 1 && weighted)
        {
            self.initialize.set_splitEncodeMode(tuning.split);
        }
        // SAFETY: `validate` checked nvEncInitializeEncoder; `initialize` and the config it points
        // to are boxed in `self`, so both addresses stay valid for the session.
        let status = unsafe {
            (self.functions.nvEncInitializeEncoder.unwrap())(self.raw, self.initialize.as_mut())
        };
        self.check(status, "initialize encoder")?;
        self.initialized = true;
        tracing::info!(
            api_major = self.api.0,
            api_minor = self.api.1,
            asynchronous,
            references = self.retained,
            rfi = self.invalidation,
            width = self.stream.width,
            height = self.stream.height,
            ten_bit = ten,
            yuv444 = self.stream.yuv444,
            "Rust native NVENC initialized"
        );
        Ok(())
    }
    fn slot(&mut self, input: Input) -> Result<usize> {
        if self.slots.len() >= MAX_PENDING {
            bail!("NVENC input registration limit reached");
        }
        let _guard = self.guard()?;
        let index = self.slots.len();
        self.slots.push(Slot {
            input,
            metadata: Metadata::default(),
            registered: ptr::null_mut(),
            mapped: ptr::null_mut(),
            output: ptr::null_mut(),
            event: HANDLE::default(),
            event_registered: false,
            signaled: false,
        });
        // Every partial acquisition is now owned by Session::drop.
        let mut register = NV_ENC_REGISTER_RESOURCE {
            version: self.api.register(),
            resourceType: self.slots[index].input.kind,
            width: self.stream.width,
            height: self.stream.height,
            pitch: self.slots[index].input.pitch,
            resourceToRegister: self.slots[index].input.raw,
            bufferFormat: buffer_format(&self.stream),
            bufferUsage: _NV_ENC_BUFFER_USAGE_NV_ENC_INPUT_IMAGE,
            ..Default::default()
        };
        // SAFETY: `validate` checked the entry; `input.raw` is kept alive by the slot's texture
        // clone or CUDA allocation until `cleanup_slot` unregisters it.
        let status =
            unsafe { (self.functions.nvEncRegisterResource.unwrap())(self.raw, &mut register) };
        self.slots[index].registered = register.registeredResource;
        self.check(status, "register GPU input")?;
        if register.registeredResource.is_null() {
            bail!("NVENC returned no registered resource");
        }
        let mut output = NV_ENC_CREATE_BITSTREAM_BUFFER {
            version: self.api.structure(1, false),
            ..Default::default()
        };
        // SAFETY: `validate` checked the entry and `output` is versioned; the buffer is owned by
        // this slot.
        let status =
            unsafe { (self.functions.nvEncCreateBitstreamBuffer.unwrap())(self.raw, &mut output) };
        self.slots[index].output = output.bitstreamBuffer;
        self.check(status, "create output buffer")?;
        if output.bitstreamBuffer.is_null() {
            bail!("NVENC returned no output buffer");
        }
        if self.initialize.enableEncodeAsync != 0 {
            // SAFETY: No pointers are passed; the handle is owned by the slot and closed in
            // `Slot::drop`.
            self.slots[index].event = unsafe { CreateEventW(None, false, false, None) }?;
            let mut event = NV_ENC_EVENT_PARAMS {
                version: self.api.event(),
                completionEvent: self.slots[index].event.0,
                ..Default::default()
            };
            self.check(
                // SAFETY: `enableEncodeAsync` is set only if nvEncRegisterAsyncEvent exists;
                // `event` is the slot's handle.
                unsafe { (self.functions.nvEncRegisterAsyncEvent.unwrap())(self.raw, &mut event) },
                "register completion event",
            )?;
            self.slots[index].event_registered = true;
        }
        Ok(index)
    }
    fn texture_slot(&mut self, texture: &ID3D11Texture2D) -> Result<usize> {
        let key = texture.as_raw() as usize;
        if let Some(index) = self.slots.iter().position(|slot| slot.input.key == key) {
            return Ok(index);
        }
        let input = if let Some(context) = &self.cuda {
            let cuda = cuda::Input::new(context, texture, self.stream.width, self.stream.height)?;
            Input {
                key,
                raw: cuda.pointer as usize as *mut c_void,
                kind: _NV_ENC_INPUT_RESOURCE_TYPE_NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR,
                pitch: cuda.pitch,
                _texture: None,
                cuda: Some(cuda),
            }
        } else {
            Input {
                key,
                raw: texture.as_raw(),
                kind: _NV_ENC_INPUT_RESOURCE_TYPE_NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX,
                pitch: 0,
                _texture: Some(texture.clone()),
                cuda: None,
            }
        };
        self.slot(input)
    }
    fn submit(
        &mut self,
        slot: usize,
        texture: Option<Arc<ID3D11Texture2D>>,
        idr: bool,
        presentation: Instant,
    ) -> Result<()> {
        let _guard = self.guard()?;
        if self.pending.len() >= MAX_PENDING || self.pending.iter().any(|p| p.slot == slot) {
            bail!("NVENC input is still in flight");
        }
        if let Some(input) = self.slots[slot].input.cuda.as_mut() {
            input.copy()?;
        }
        let mut map = NV_ENC_MAP_INPUT_RESOURCE {
            version: self.api.structure(4, false),
            registeredResource: self.slots[slot].registered,
            ..Default::default()
        };
        // SAFETY: `registered` is this slot's live registration and no pending frame has it mapped
        // (checked above).
        let status = unsafe { (self.functions.nvEncMapInputResource.unwrap())(self.raw, &mut map) };
        self.slots[slot].mapped = map.mappedResource;
        self.check(status, "map encoder input")?;
        if map.mappedResource.is_null() || map.mappedBufferFmt != buffer_format(&self.stream) {
            bail!("NVENC mapped an incompatible input");
        }
        let idr = idr || std::mem::take(&mut self.force_idr);
        let mut picture = NV_ENC_PIC_PARAMS {
            version: self.api.picture(),
            inputWidth: self.stream.width,
            inputHeight: self.stream.height,
            inputPitch: self.slots[slot].input.pitch,
            frameIdx: self.next as u32,
            inputTimeStamp: self.next,
            inputBuffer: map.mappedResource,
            outputBitstream: self.slots[slot].output,
            completionEvent: self.slots[slot].event.0,
            bufferFmt: map.mappedBufferFmt,
            pictureStruct: _NV_ENC_PIC_STRUCT_NV_ENC_PIC_STRUCT_FRAME,
            encodePicFlags: if idr {
                _NV_ENC_PIC_FLAGS_NV_ENC_PIC_FLAG_FORCEIDR
                    | _NV_ENC_PIC_FLAGS_NV_ENC_PIC_FLAG_OUTPUT_SPSPPS
            } else {
                0
            },
            ..Default::default()
        };
        if self.stream.hdr && self.api >= ApiVersion(13, 0) {
            *self.slots[slot].metadata.mastering = *self.metadata.mastering;
            *self.slots[slot].metadata.light = *self.metadata.light;
            {
                if self.stream.codec == 1 {
                    picture.codecPicParams.hevcPicParams.pMasteringDisplay =
                        self.slots[slot].metadata.mastering.as_mut();
                    picture.codecPicParams.hevcPicParams.pMaxCll =
                        self.slots[slot].metadata.light.as_mut();
                } else if self.stream.codec == 2 {
                    picture.codecPicParams.av1PicParams.pMasteringDisplay =
                        self.slots[slot].metadata.mastering.as_mut();
                    picture.codecPicParams.av1PicParams.pMaxCll =
                        self.slots[slot].metadata.light.as_mut();
                }
            }
        }
        let started = Instant::now();
        // SAFETY: `picture` points at the slot's mapped input, output buffer and boxed metadata,
        // which all stay alive while the slot is in `pending`.
        let status =
            unsafe { (self.functions.nvEncEncodePicture.unwrap())(self.raw, &mut picture) };
        if status != SUCCESS && status != _NVENCSTATUS_NV_ENC_ERR_NEED_MORE_INPUT {
            self.force_idr |= idr;
            self.check(status, "submit picture")?;
        }
        let after_invalidation = self.references.confirmation(idr);
        self.slots[slot].signaled = false;
        self.pending.push_back(Pending {
            slot,
            frame: self.next,
            started,
            presentation,
            after_invalidation,
            _converted: texture,
        });
        self.next = self
            .next
            .checked_add(1)
            .context("NVENC frame index overflow")?;
        Ok(())
    }
    fn collect(&mut self) -> Result<()> {
        let _guard = self.guard()?;
        while let Some(pending) = self.pending.front() {
            let index = pending.slot;
            let slot = &mut self.slots[index];
            if !slot.event.is_invalid() && !slot.signaled {
                // SAFETY: `slot.event` is a valid handle (checked above) owned by the slot.
                match unsafe { WaitForSingleObject(slot.event, 0) } {
                    WAIT_OBJECT_0 => slot.signaled = true,
                    WAIT_TIMEOUT => {
                        if pending.started.elapsed() > COMPLETION_TIMEOUT {
                            bail!("NVENC completion timed out");
                        }
                        break;
                    }
                    _ => bail!("NVENC completion event failed"),
                }
            }
            let mut lock = NV_ENC_LOCK_BITSTREAM {
                version: self.api.lock(),
                outputBitstream: slot.output,
                ..Default::default()
            };
            lock.set_doNotWait(u32::from(!slot.event.is_invalid()));
            // SAFETY: `slot.output` is the pending slot's bitstream buffer on this session and
            // `lock` is versioned.
            let status =
                unsafe { (self.functions.nvEncLockBitstream.unwrap())(self.raw, &mut lock) };
            if status == _NVENCSTATUS_NV_ENC_ERR_LOCK_BUSY {
                if pending.started.elapsed() > COMPLETION_TIMEOUT {
                    bail!("NVENC output lock timed out");
                }
                break;
            }
            self.check(status, "lock completed output")?;
            let result = (|| -> Result<Encoded> {
                if lock.outputTimeStamp != pending.frame
                    || lock.bitstreamBufferPtr.is_null()
                    || lock.bitstreamSizeInBytes == 0
                    || lock.bitstreamSizeInBytes as usize > MAX_FRAME_BYTES
                {
                    bail!("invalid NVENC output timestamp or size");
                }
                let idr = lock.pictureType == _NV_ENC_PIC_TYPE_NV_ENC_PIC_TYPE_IDR;
                Ok(Encoded {
                    // SAFETY: The lock succeeded and the pointer is non-null with a bounded size;
                    // the bytes stay valid until the unlock below.
                    bytes: unsafe {
                        std::slice::from_raw_parts(
                            lock.bitstreamBufferPtr.cast(),
                            lock.bitstreamSizeInBytes as usize,
                        )
                    }
                    .to_vec(),
                    idr,
                    after_invalidation: pending.after_invalidation && !idr,
                    latency: Some(pending.started.elapsed()),
                    presentation: Some(pending.presentation),
                })
            })();
            self.check(
                // SAFETY: `output` was locked by the successful nvEncLockBitstream above.
                unsafe {
                    (self.functions.nvEncUnlockBitstream.unwrap())(
                        self.raw,
                        self.slots[index].output,
                    )
                },
                "unlock output",
            )?;
            let encoded = result?;
            self.check(
                // SAFETY: `mapped` is the input mapped for this frame in `submit`, and its output
                // has been collected.
                unsafe {
                    (self.functions.nvEncUnmapInputResource.unwrap())(
                        self.raw,
                        self.slots[index].mapped,
                    )
                },
                "release completed input",
            )?;
            self.slots[index].mapped = ptr::null_mut();
            let pending = self.pending.pop_front().unwrap();
            self.references.completed(pending.frame, encoded.idr);
            self.ready.push_back(encoded);
        }
        Ok(())
    }
    fn drain(&mut self) -> Result<()> {
        let deadline = Instant::now() + COMPLETION_TIMEOUT;
        while !self.pending.is_empty() {
            self.collect()?;
            if self.pending.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                bail!("NVENC drain timed out");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
    fn clear_inputs(&mut self) -> Result<()> {
        self.drain()?;
        let _guard = self.guard()?;
        for slot in &mut self.slots {
            cleanup_slot(&self.functions, self.raw, self.api, slot)?;
        }
        self.slots.clear();
        Ok(())
    }
    fn bitrate(&mut self, bitrate: u32) -> Result<()> {
        if bitrate == self.stream.bitrate_kbps {
            return Ok(());
        }
        if bitrate == 0 || bitrate > 800_000 {
            bail!("NVENC bitrate out of range");
        }
        if !self.dynamic_bitrate {
            bail!("NVENC driver cannot change bitrate in this session");
        }
        self.drain()?;
        let _guard = self.guard()?;
        let mut config = Box::new(*self.config);
        let before = config.rcParams.averageBitRate;
        let after = bitrate * 1000;
        config.rcParams.averageBitRate = after;
        config.rcParams.maxBitRate = after;
        if config.rcParams.vbvBufferSize > 0 && before > 0 {
            config.rcParams.vbvBufferSize =
                (u64::from(after) * u64::from(config.rcParams.vbvBufferSize) / u64::from(before))
                    .clamp(100_000, u64::from(u32::MAX)) as u32;
        }
        let mut reconfigure = NV_ENC_RECONFIGURE_PARAMS {
            version: self.api.reconfigure(),
            reInitEncodeParams: *self.initialize,
            ..Default::default()
        };
        reconfigure.reInitEncodeParams.encodeConfig = config.as_mut();
        reconfigure.set_resetEncoder(u32::from(after > before));
        reconfigure.set_forceIDR(u32::from(after > before));
        self.check(
            // SAFETY: `dynamic_bitrate` requires nvEncReconfigureEncoder, and the boxed `config` is
            // kept as `self.config` after the call.
            unsafe {
                (self.functions.nvEncReconfigureEncoder.unwrap())(self.raw, &mut reconfigure)
            },
            "change bitrate",
        )?;
        self.config = config;
        self.initialize.encodeConfig = self.config.as_mut();
        self.stream.bitrate_kbps = bitrate;
        self.force_idr |= after > before;
        Ok(())
    }
    fn invalidate(&mut self, first: u64, last: u64) -> Result<bool> {
        self.drain()?;
        let _guard = self.guard()?;
        match self
            .references
            .plan(first, last, self.retained, self.invalidation)
        {
            Recovery::AlreadyApplied => Ok(true),
            Recovery::Idr => Ok(false),
            Recovery::Invalidate { first, last } => {
                for frame in first..=last {
                    self.check(
                        // SAFETY: `plan` returns Invalidate only when `self.invalidation`, which
                        // requires this entry, is set.
                        unsafe {
                            (self.functions.nvEncInvalidateRefFrames.unwrap())(self.raw, frame)
                        },
                        "invalidate lost reference",
                    )?;
                }
                self.references.applied(first, last);
                Ok(true)
            }
        }
    }
    fn poll(&mut self) -> Result<Vec<Encoded>> {
        self.collect()?;
        Ok(self.ready.drain(..).collect())
    }
}
fn cleanup_slot(
    functions: &NV_ENCODE_API_FUNCTION_LIST,
    raw: *mut c_void,
    api: ApiVersion,
    slot: &mut Slot,
) -> Result<()> {
    let check = |status, operation| check_raw(status, operation, functions, raw);
    // SAFETY: `raw` is the session these resources belong to; each is released only while still
    // held and then cleared, and the unregister entry existed when `event_registered` was set.
    unsafe {
        if !slot.mapped.is_null() {
            check(
                (functions.nvEncUnmapInputResource.unwrap())(raw, slot.mapped),
                "unmap input during cleanup",
            )?;
            slot.mapped = ptr::null_mut();
        }
        if slot.event_registered {
            let mut event = NV_ENC_EVENT_PARAMS {
                version: api.event(),
                completionEvent: slot.event.0,
                ..Default::default()
            };
            check(
                (functions.nvEncUnregisterAsyncEvent.unwrap())(raw, &mut event),
                "unregister completion event",
            )?;
            slot.event_registered = false;
        }
        if !slot.output.is_null() {
            check(
                (functions.nvEncDestroyBitstreamBuffer.unwrap())(raw, slot.output),
                "destroy output buffer",
            )?;
            slot.output = ptr::null_mut();
        }
        if !slot.registered.is_null() {
            check(
                (functions.nvEncUnregisterResource.unwrap())(raw, slot.registered),
                "unregister GPU input",
            )?;
            slot.registered = ptr::null_mut();
        }
    }
    Ok(())
}
impl Drop for Session {
    fn drop(&mut self) {
        if self.raw.is_null() {
            return;
        }
        let _guard = match self.guard() {
            Ok(guard) => guard,
            Err(error) => {
                tracing::warn!(%error, "CUDA device lost during NVENC teardown; attempting driver cleanup");
                None
            }
        };
        if self.initialized && !self.pending.is_empty() {
            let mut eos = NV_ENC_PIC_PARAMS {
                version: self.api.picture(),
                encodePicFlags: _NV_ENC_PIC_FLAGS_NV_ENC_PIC_FLAG_EOS,
                ..Default::default()
            };
            // SAFETY: `self.raw` is the non-null open session and `eos` is versioned.
            let _ = unsafe { (self.functions.nvEncEncodePicture.unwrap())(self.raw, &mut eos) };
            let _ = self.drain();
        }
        for (index, slot) in self.slots.iter_mut().enumerate() {
            // A failed drain does not establish completion. Let session
            // destruction stop the native worker before releasing its input,
            // output buffer or completion event.
            if self.pending.iter().any(|pending| pending.slot == index) {
                continue;
            }
            if let Err(error) = cleanup_slot(&self.functions, self.raw, self.api, slot) {
                tracing::warn!(%error, "NVENC input cleanup deferred to session destruction");
            }
        }
        // SAFETY: `self.raw` is non-null (checked at the top) and is destroyed once, since it is
        // cleared below.
        let status = unsafe { (self.functions.nvEncDestroyEncoder.unwrap())(self.raw) };
        if let Err(error) = self.check(status, "destroy session") {
            // A failed native destroy leaves its async workers' lifetime
            // unknown. Preserve their input/context/driver owners until process
            // exit instead of freeing resources behind a still-running codec.
            tracing::error!(%error, "NVENC cleanup failed; driver resources retained until host exit");
            std::mem::forget(std::mem::take(&mut self.slots));
            std::mem::forget(std::mem::take(&mut self.pending));
            std::mem::forget(self._runtime.clone());
            if let Some(context) = self.cuda.take() {
                std::mem::forget(context);
            }
        }
        self.raw = ptr::null_mut();
        // Drop CUDA allocations/textures only after the driver destroys the
        // session, including inputs whose explicit unregister was rejected.
        self.slots.clear();
        self.pending.clear();
    }
}

pub struct Encoder {
    session: Session,
    gpu: Device,
    converter: Option<crate::gpu_color::Converter>,
    source: Option<(u32, u32, Pixel)>,
    luminance: [f32; 2],
}
impl Encoder {
    pub fn new_device_options(config: &Negotiated, gpu: Device, options: &Config) -> Result<Self> {
        let runtime = Runtime::load()?;
        let tuning = Tuning::new(options, config)?;
        let cuda = if config.yuv444 && config.ten_bit() {
            Some(cuda::Context::new(&gpu)?)
        } else {
            None
        };
        let (device, kind) = cuda
            .as_ref()
            .map(|context| (context.raw(), _NV_ENC_DEVICE_TYPE_NV_ENC_DEVICE_TYPE_CUDA))
            .unwrap_or((
                gpu.device.as_raw(),
                _NV_ENC_DEVICE_TYPE_NV_ENC_DEVICE_TYPE_DIRECTX,
            ));
        let mut session = Session::open(runtime, device, kind, cuda, config, &tuning)?;
        session.metadata.update(gpu.hdr_metadata());
        Ok(Self {
            session,
            gpu,
            converter: None,
            source: None,
            luminance: [100., 1.],
        })
    }
    pub fn set_luminance(&mut self, luminance: [f32; 2]) {
        self.luminance = luminance;
    }
    pub fn set_hdr_metadata(&mut self, metadata: butterpollo_core::hdr::Metadata) {
        self.session.metadata.update(metadata);
    }
    pub fn set_next_frame(&mut self, frame: u64) {
        debug_assert!(self.session.pending.is_empty() && self.session.ready.is_empty());
        self.session.next = frame.max(1);
    }
    pub fn supports_invalidation(&self) -> bool {
        self.session.invalidation
    }
    pub fn invalidate_ref_frames(&mut self, first: u64, last: u64) -> bool {
        match self.session.invalidate(first, last) {
            Ok(applied) => applied,
            Err(error) => {
                tracing::warn!(%error, "NVENC reference recovery failed; requesting a larger IDR recovery frame. Reduce bitrate on lossy links or update the NVIDIA driver");
                self.session.force_idr = true;
                false
            }
        }
    }
    pub fn pending(&self) -> bool {
        !self.session.pending.is_empty() || !self.session.ready.is_empty()
    }
    /// Frames submitted and not yet encoded.
    pub fn backlog(&self) -> usize {
        self.session.pending.len()
    }
    pub fn poll(&mut self) -> Result<Vec<Encoded>> {
        self.session.poll()
    }
    pub fn accepts_gpu_device(&self, image: &GpuImage) -> bool {
        self.gpu.device.as_raw() == image.gpu.device.as_raw()
    }
    pub fn encode(&mut self, image: &Image, idr: bool, bitrate: u32) -> Result<Vec<Encoded>> {
        let gpu = GpuImage::upload(&self.gpu, image)?;
        self.encode_gpu(&gpu, idr, bitrate)
    }
    pub fn encode_gpu(
        &mut self,
        image: &GpuImage,
        idr: bool,
        bitrate: u32,
    ) -> Result<Vec<Encoded>> {
        if !self.accepts_gpu_device(image) {
            bail!("NVENC capture device changed; recreate the encoder");
        }
        self.session.collect()?;
        self.session.bitrate(bitrate)?;
        if self.session.pending.len() >= MAX_PENDING {
            self.session.drain()?;
        }
        let source = (image.width, image.height, image.pixel);
        if self.source != Some(source) {
            self.session.clear_inputs()?;
            self.converter = Some(crate::gpu_color::Converter::new(
                &self.gpu,
                &self.session.stream,
                source,
            )?);
            self.source = Some(source);
        }
        let converter = self.converter.as_mut().unwrap();
        converter.set_luminance(self.luminance);
        let converted = converter.convert(image)?;
        let slot = self.session.texture_slot(converted.as_ref())?;
        self.session
            .submit(slot, Some(converted), idr, image.captured)?;
        self.session.poll()
    }
}
#[cfg(test)]
mod tests;
