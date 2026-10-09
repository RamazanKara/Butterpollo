//! A driver function table exercises the real session owner and submission
//! path without requiring NVIDIA hardware. Hardware decode is a separate test.

use super::*;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
};
use windows::Win32::System::Threading::SetEvent;

#[derive(Default)]
struct Buffer {
    bytes: Vec<u8>,
    timestamp: u64,
    idr: bool,
    locked: bool,
    metadata: Option<(*const MASTERING_DISPLAY_INFO, u32)>,
}
#[derive(Default)]
struct Driver {
    opened: Vec<ApiVersion>,
    destroyed: Vec<ApiVersion>,
    active: HashSet<usize>,
    resources: HashMap<usize, NV_ENC_BUFFER_FORMAT>,
    mapped: HashSet<usize>,
    outputs: HashMap<usize, Box<Buffer>>,
    events: HashSet<usize>,
    calls: Vec<&'static str>,
    invalidated: Vec<u64>,
    initialized: Option<(NV_ENC_INITIALIZE_PARAMS, NV_ENC_CONFIG)>,
    reconfigured: Option<(NV_ENC_RECONFIGURE_PARAMS, NV_ENC_CONFIG)>,
    reject_open: Option<ApiVersion>,
    reject_initialize: Option<ApiVersion>,
    reject_output: bool,
    reject_reconfigure: bool,
    reject_invalidate: Option<u64>,
    asynchronous: bool,
    no_rfi: bool,
    no_multiple_refs: bool,
    preset_fallback: bool,
    lock_busy: usize,
    need_more_input: bool,
    destroy_busy: bool,
}
thread_local! { static DRIVER: RefCell<Driver> = RefCell::default(); }
fn state<T>(call: impl FnOnce(&mut Driver) -> T) -> T {
    DRIVER.with(|d| call(&mut d.borrow_mut()))
}
fn reset() {
    state(|d| *d = Driver::default());
}
fn version(value: u32) -> ApiVersion {
    ApiVersion((value & 0xff) as u8, (value >> 24 & 0xf) as u8)
}
unsafe fn handle(raw: *mut c_void) -> ApiVersion {
    // SAFETY: Callers pass a session handle that `open` boxed from an ApiVersion and `destroy` has
    // not freed.
    unsafe { *raw.cast::<ApiVersion>() }
}

unsafe extern "C" fn create(table: *mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS {
    // SAFETY: `Session::open_version` passes its live, exclusive function table.
    let table = unsafe { &mut *table };
    assert_eq!(table.version, version(table.version).structure(2, false));
    table.nvEncOpenEncodeSessionEx = Some(open);
    table.nvEncGetEncodeCaps = Some(caps);
    table.nvEncGetEncodePresetConfigEx = Some(preset_ex);
    table.nvEncGetEncodePresetConfig = Some(preset);
    table.nvEncInitializeEncoder = Some(initialize);
    table.nvEncRegisterResource = Some(register);
    table.nvEncUnregisterResource = Some(unregister);
    table.nvEncMapInputResource = Some(map);
    table.nvEncUnmapInputResource = Some(unmap);
    table.nvEncCreateBitstreamBuffer = Some(output);
    table.nvEncDestroyBitstreamBuffer = Some(destroy_output);
    table.nvEncRegisterAsyncEvent = Some(register_event);
    table.nvEncUnregisterAsyncEvent = Some(unregister_event);
    table.nvEncEncodePicture = Some(encode);
    table.nvEncLockBitstream = Some(lock);
    table.nvEncUnlockBitstream = Some(unlock);
    table.nvEncReconfigureEncoder = Some(reconfigure);
    table.nvEncInvalidateRefFrames = Some(invalidate);
    table.nvEncDestroyEncoder = Some(destroy);
    SUCCESS
}
unsafe extern "C" fn open(
    parameters: *mut NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS,
    raw: *mut *mut c_void,
) -> NVENCSTATUS {
    // SAFETY: The session passes a pointer to its live parameter struct for this call.
    let p = unsafe { &*parameters };
    let api = version(p.apiVersion);
    assert_eq!(p.version, api.structure(1, false));
    // SAFETY: `raw` points at the session's `raw` field, writable for this call.
    unsafe {
        *raw = Box::into_raw(Box::new(api)).cast();
    }
    state(|d| {
        d.opened.push(api);
        // SAFETY: `raw` was written just above and still points at the session's field.
        d.active.insert(unsafe { *raw } as usize);
        if d.reject_open == Some(api) {
            _NVENCSTATUS_NV_ENC_ERR_INVALID_VERSION
        } else {
            SUCCESS
        }
    })
}
unsafe extern "C" fn destroy(raw: *mut c_void) -> NVENCSTATUS {
    // SAFETY: `raw` is a live handle from `open`; it is freed only at the end of this call.
    let api = unsafe { handle(raw) };
    state(|d| {
        assert!(d.active.remove(&(raw as usize)));
        if d.destroy_busy {
            assert!(!d.mapped.is_empty());
            assert!(!d.resources.is_empty());
            assert!(!d.outputs.is_empty());
            assert!(!d.events.is_empty());
            assert!(!d.calls.iter().any(|call| matches!(
                *call,
                "unmap input" | "unregister event" | "destroy output" | "unregister input"
            )));
            // Model the driver stopping its worker and releasing native
            // registrations only when the session itself is destroyed.
            d.mapped.clear();
            d.resources.clear();
            d.outputs.clear();
            d.events.clear();
        }
        d.destroyed.push(api);
        d.calls.push("destroy session");
    });
    // SAFETY: `raw` came from Box::into_raw in `open`, and each session is destroyed once.
    drop(unsafe { Box::from_raw(raw.cast::<ApiVersion>()) });
    SUCCESS
}
unsafe extern "C" fn caps(
    raw: *mut c_void,
    _: GUID,
    parameters: *mut NV_ENC_CAPS_PARAM,
    value: *mut i32,
) -> NVENCSTATUS {
    // SAFETY: The session passes a pointer to its live parameter struct for this call.
    let p = unsafe { &*parameters };
    // SAFETY: `raw` is a live handle that `open` boxed and `destroy` has not freed.
    assert_eq!(p.version, unsafe { handle(raw) }.structure(1, false));
    // SAFETY: `value` points at the session's live local out-value.
    unsafe {
        *value = state(|d| match p.capsToQuery {
            _NV_ENC_CAPS_NV_ENC_CAPS_WIDTH_MAX | _NV_ENC_CAPS_NV_ENC_CAPS_HEIGHT_MAX => 8192,
            _NV_ENC_CAPS_NV_ENC_CAPS_ASYNC_ENCODE_SUPPORT => i32::from(d.asynchronous),
            _NV_ENC_CAPS_NV_ENC_CAPS_SUPPORT_REF_PIC_INVALIDATION => i32::from(!d.no_rfi),
            _NV_ENC_CAPS_NV_ENC_CAPS_SUPPORT_MULTIPLE_REF_FRAMES => i32::from(!d.no_multiple_refs),
            _ => 1,
        });
    }
    SUCCESS
}
unsafe extern "C" fn preset(
    raw: *mut c_void,
    _: GUID,
    _: GUID,
    config: *mut NV_ENC_PRESET_CONFIG,
) -> NVENCSTATUS {
    // SAFETY: `raw` is a live handle that `open` boxed and `destroy` has not freed.
    let api = unsafe { handle(raw) };
    // SAFETY: The session passes an exclusive pointer to its live preset struct.
    let p = unsafe { &mut *config };
    assert_eq!(p.version, api.preset());
    assert_eq!(p.presetCfg.version, api.config());
    // A quality preset may enable B frames/lookahead. The host must override it.
    p.presetCfg.frameIntervalP = 4;
    p.presetCfg.rcParams.set_enableLookahead(1);
    p.presetCfg.rcParams.lookaheadDepth = 16;
    state(|d| d.calls.push("preset"));
    SUCCESS
}
unsafe extern "C" fn preset_ex(
    raw: *mut c_void,
    codec: GUID,
    guid: GUID,
    tuning: NV_ENC_TUNING_INFO,
    config: *mut NV_ENC_PRESET_CONFIG,
) -> NVENCSTATUS {
    assert_eq!(
        tuning,
        NV_ENC_TUNING_INFO_NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY
    );
    if state(|d| d.preset_fallback) {
        _NVENCSTATUS_NV_ENC_ERR_UNSUPPORTED_PARAM
    } else {
        // SAFETY: The arguments are this call's own, valid as `preset` requires.
        unsafe { preset(raw, codec, guid, config) }
    }
}
unsafe extern "C" fn initialize(
    raw: *mut c_void,
    parameters: *mut NV_ENC_INITIALIZE_PARAMS,
) -> NVENCSTATUS {
    // SAFETY: The session passes a pointer to its live parameter struct for this call.
    let p = unsafe { *parameters };
    // SAFETY: `raw` is a live handle that `open` boxed and `destroy` has not freed.
    let api = unsafe { handle(raw) };
    assert_eq!(p.version, api.initialize());
    assert!(!p.encodeConfig.is_null());
    // SAFETY: `encodeConfig` is non-null (asserted above) and points at the session's boxed config.
    let config = unsafe { *p.encodeConfig };
    assert_eq!(config.version, api.config());
    state(|d| {
        d.initialized = Some((p, config));
        if d.reject_initialize == Some(api) {
            _NVENCSTATUS_NV_ENC_ERR_INVALID_VERSION
        } else {
            SUCCESS
        }
    })
}
unsafe extern "C" fn register(
    raw: *mut c_void,
    parameters: *mut NV_ENC_REGISTER_RESOURCE,
) -> NVENCSTATUS {
    // SAFETY: The session passes an exclusive pointer to its live parameter struct for this call.
    let p = unsafe { &mut *parameters };
    // SAFETY: `raw` is a live handle that `open` boxed and `destroy` has not freed.
    assert_eq!(p.version, unsafe { handle(raw) }.register());
    p.registeredResource = p.resourceToRegister;
    state(|d| {
        assert!(
            d.resources
                .insert(p.registeredResource as usize, p.bufferFormat)
                .is_none()
        );
        d.calls.push("register input");
    });
    SUCCESS
}
unsafe extern "C" fn unregister(_: *mut c_void, resource: NV_ENC_REGISTERED_PTR) -> NVENCSTATUS {
    state(|d| {
        assert!(!d.mapped.contains(&(resource as usize)));
        assert!(d.resources.remove(&(resource as usize)).is_some());
        d.calls.push("unregister input");
    });
    SUCCESS
}
unsafe extern "C" fn map(
    raw: *mut c_void,
    parameters: *mut NV_ENC_MAP_INPUT_RESOURCE,
) -> NVENCSTATUS {
    // SAFETY: The session passes an exclusive pointer to its live parameter struct for this call.
    let p = unsafe { &mut *parameters };
    // SAFETY: `raw` is a live handle that `open` boxed and `destroy` has not freed.
    assert_eq!(p.version, unsafe { handle(raw) }.structure(4, false));
    p.mappedResource = p.registeredResource;
    state(|d| {
        p.mappedBufferFmt = d.resources[&(p.registeredResource as usize)];
        assert!(d.mapped.insert(p.mappedResource as usize));
        d.calls.push("map input");
    });
    SUCCESS
}
unsafe extern "C" fn unmap(_: *mut c_void, resource: NV_ENC_INPUT_PTR) -> NVENCSTATUS {
    state(|d| {
        assert!(d.mapped.remove(&(resource as usize)));
        d.calls.push("unmap input");
    });
    SUCCESS
}
unsafe extern "C" fn output(
    raw: *mut c_void,
    parameters: *mut NV_ENC_CREATE_BITSTREAM_BUFFER,
) -> NVENCSTATUS {
    // SAFETY: The session passes an exclusive pointer to its live parameter struct for this call.
    let p = unsafe { &mut *parameters };
    // SAFETY: `raw` is a live handle that `open` boxed and `destroy` has not freed.
    assert_eq!(p.version, unsafe { handle(raw) }.structure(1, false));
    let mut buffer = Box::<Buffer>::default();
    p.bitstreamBuffer = (&mut *buffer as *mut Buffer).cast();
    state(|d| {
        d.outputs.insert(p.bitstreamBuffer as usize, buffer);
        d.calls.push("create output");
        if d.reject_output {
            _NVENCSTATUS_NV_ENC_ERR_OUT_OF_MEMORY
        } else {
            SUCCESS
        }
    })
}
unsafe extern "C" fn destroy_output(_: *mut c_void, output: NV_ENC_OUTPUT_PTR) -> NVENCSTATUS {
    state(|d| {
        let buffer = d.outputs.remove(&(output as usize)).unwrap();
        assert!(!buffer.locked);
        d.calls.push("destroy output");
    });
    SUCCESS
}
unsafe extern "C" fn register_event(
    raw: *mut c_void,
    parameters: *mut NV_ENC_EVENT_PARAMS,
) -> NVENCSTATUS {
    // SAFETY: The session passes a pointer to its live parameter struct for this call.
    let p = unsafe { &*parameters };
    // SAFETY: `raw` is a live handle that `open` boxed and `destroy` has not freed.
    assert_eq!(p.version, unsafe { handle(raw) }.event());
    state(|d| assert!(d.events.insert(p.completionEvent as usize)));
    SUCCESS
}
unsafe extern "C" fn unregister_event(
    raw: *mut c_void,
    parameters: *mut NV_ENC_EVENT_PARAMS,
) -> NVENCSTATUS {
    // SAFETY: The session passes a pointer to its live parameter struct for this call.
    let p = unsafe { &*parameters };
    // SAFETY: `raw` is a live handle that `open` boxed and `destroy` has not freed.
    assert_eq!(p.version, unsafe { handle(raw) }.event());
    state(|d| {
        assert!(d.events.remove(&(p.completionEvent as usize)));
        d.calls.push("unregister event");
    });
    SUCCESS
}
unsafe extern "C" fn encode(raw: *mut c_void, parameters: *mut NV_ENC_PIC_PARAMS) -> NVENCSTATUS {
    // SAFETY: The session passes a pointer to its live parameter struct for this call.
    let p = unsafe { &*parameters };
    // SAFETY: `raw` is a live handle that `open` boxed and `destroy` has not freed.
    assert_eq!(p.version, unsafe { handle(raw) }.picture());
    if p.encodePicFlags & _NV_ENC_PIC_FLAGS_NV_ENC_PIC_FLAG_EOS != 0 {
        return SUCCESS;
    }
    state(|d| {
        assert!(d.mapped.contains(&(p.inputBuffer as usize)));
        let codec = state_codec(d);
        let buffer = d.outputs.get_mut(&(p.outputBitstream as usize)).unwrap();
        buffer.bytes = p.inputTimeStamp.to_le_bytes().to_vec();
        buffer.timestamp = p.inputTimeStamp;
        buffer.idr = p.encodePicFlags & _NV_ENC_PIC_FLAGS_NV_ENC_PIC_FLAG_FORCEIDR != 0;
        let pointer = if codec == 1 {
            // SAFETY: An HEVC session fills this union member; it holds only integers and pointers.
            unsafe { p.codecPicParams.hevcPicParams.pMasteringDisplay }
        } else if codec == 2 {
            // SAFETY: An AV1 session fills this union member; it holds only integers and pointers.
            unsafe { p.codecPicParams.av1PicParams.pMasteringDisplay }
        } else {
            ptr::null_mut()
        };
        if !pointer.is_null() {
            // SAFETY: `pointer` is non-null and points at the slot's boxed metadata, live while the
            // frame is pending.
            buffer.metadata = Some((pointer, unsafe { (*pointer).maxLuma }));
        }
        if !p.completionEvent.is_null() {
            assert!(d.events.contains(&(p.completionEvent as usize)));
            // SAFETY: `completionEvent` is the slot's live event, registered with this driver
            // (asserted above).
            unsafe {
                SetEvent(HANDLE(p.completionEvent)).unwrap();
            }
        }
        if d.need_more_input {
            _NVENCSTATUS_NV_ENC_ERR_NEED_MORE_INPUT
        } else {
            SUCCESS
        }
    })
}
unsafe extern "C" fn lock(raw: *mut c_void, parameters: *mut NV_ENC_LOCK_BITSTREAM) -> NVENCSTATUS {
    // SAFETY: The session passes an exclusive pointer to its live parameter struct for this call.
    let p = unsafe { &mut *parameters };
    // SAFETY: `raw` is a live handle that `open` boxed and `destroy` has not freed.
    assert_eq!(p.version, unsafe { handle(raw) }.lock());
    state(|d| {
        if d.lock_busy > 0 {
            d.lock_busy -= 1;
            return _NVENCSTATUS_NV_ENC_ERR_LOCK_BUSY;
        }
        let buffer = d.outputs.get_mut(&(p.outputBitstream as usize)).unwrap();
        if let Some((pointer, expected)) = buffer.metadata {
            assert_eq!(
                // SAFETY: The slot owning `pointer` is still pending, so its metadata box is live.
                unsafe { (*pointer).maxLuma },
                expected,
                "in-flight HDR metadata was overwritten"
            );
        }
        assert!(!buffer.locked);
        buffer.locked = true;
        p.bitstreamBufferPtr = buffer.bytes.as_mut_ptr().cast();
        p.bitstreamSizeInBytes = buffer.bytes.len() as u32;
        p.outputTimeStamp = buffer.timestamp;
        p.pictureType = if buffer.idr {
            _NV_ENC_PIC_TYPE_NV_ENC_PIC_TYPE_IDR
        } else {
            _NV_ENC_PIC_TYPE_NV_ENC_PIC_TYPE_P
        };
        d.calls.push("lock output");
        SUCCESS
    })
}
unsafe extern "C" fn unlock(_: *mut c_void, output: NV_ENC_OUTPUT_PTR) -> NVENCSTATUS {
    state(|d| {
        let buffer = d.outputs.get_mut(&(output as usize)).unwrap();
        assert!(buffer.locked);
        buffer.locked = false;
        d.calls.push("unlock output");
    });
    SUCCESS
}
unsafe extern "C" fn reconfigure(
    raw: *mut c_void,
    parameters: *mut NV_ENC_RECONFIGURE_PARAMS,
) -> NVENCSTATUS {
    // SAFETY: The session passes a pointer to its live parameter struct for this call.
    let p = unsafe { *parameters };
    // SAFETY: `raw` is a live handle that `open` boxed and `destroy` has not freed.
    assert_eq!(p.version, unsafe { handle(raw) }.reconfigure());
    // SAFETY: `encodeConfig` is the boxed config `Session::bitrate` keeps alive for this call.
    let config = unsafe { *p.reInitEncodeParams.encodeConfig };
    state(|d| {
        d.reconfigured = Some((p, config));
        if d.reject_reconfigure {
            _NVENCSTATUS_NV_ENC_ERR_INVALID_PARAM
        } else {
            SUCCESS
        }
    })
}
unsafe extern "C" fn invalidate(_: *mut c_void, frame: u64) -> NVENCSTATUS {
    state(|d| {
        if d.reject_invalidate == Some(frame) {
            _NVENCSTATUS_NV_ENC_ERR_INVALID_PARAM
        } else {
            d.invalidated.push(frame);
            SUCCESS
        }
    })
}
fn session(config: &Negotiated, maximum: ApiVersion) -> Result<Session> {
    Session::open(
        Arc::new(Runtime {
            _library: None,
            create,
            maximum,
        }),
        ptr::dangling_mut::<c_void>(),
        _NV_ENC_DEVICE_TYPE_NV_ENC_DEVICE_TYPE_DIRECTX,
        None,
        config,
        &Tuning::new(&Config::default(), config)?,
    )
}
fn input(session: &mut Session, key: usize) -> Result<usize> {
    session.slot(Input {
        key,
        raw: key as *mut c_void,
        kind: _NV_ENC_INPUT_RESOURCE_TYPE_NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX,
        pitch: 0,
        _texture: None,
        cuda: None,
    })
}
fn assert_clean() {
    state(|d| {
        assert!(d.active.is_empty());
        assert!(d.resources.is_empty());
        assert!(d.outputs.is_empty());
        assert!(d.events.is_empty());
        assert!(d.mapped.is_empty());
    });
}
fn state_codec(driver: &Driver) -> u8 {
    let guid = driver.initialized.as_ref().unwrap().0.encodeGUID;
    if guid.Data1 == NV_ENC_CODEC_H264_GUID.Data1 {
        0
    } else if guid.Data1 == NV_ENC_CODEC_HEVC_GUID.Data1 {
        1
    } else {
        2
    }
}

#[test]
fn rejected_versions_destroy_partial_sessions_and_keep_legacy_ten_bit_fields() -> Result<()> {
    reset();
    state(|d| {
        d.reject_open = Some(ApiVersion(13, 0));
        d.reject_initialize = Some(ApiVersion(12, 2));
        d.preset_fallback = true;
    });
    let config = Negotiated {
        codec: 1,
        hdr: true,
        rate_millihz: 119880,
        ..Default::default()
    };
    let encoder = session(&config, ApiVersion(13, 0))?;
    assert_eq!(encoder.api, ApiVersion(12, 1));
    // SAFETY: The session configured HEVC; the union member holds only integers and pointers.
    let format = unsafe { encoder.config.encodeCodecConfig.hevcConfig };
    assert_eq!(format.reserved3(), 2);
    assert_eq!(format.inputBitDepth, 0);
    assert_eq!(format.outputBitDepth, 0);
    assert_eq!(format.hevcVUIParameters.colourPrimaries, 9);
    assert_eq!(format.hevcVUIParameters.transferCharacteristics, 16);
    assert_eq!(encoder.initialize.frameRateNum, 119880);
    assert_eq!(encoder.initialize.frameRateDen, 1000);
    assert_eq!(encoder.config.frameIntervalP, 1);
    assert_eq!(encoder.config.rcParams.enableLookahead(), 0);
    state(|d| {
        assert_eq!(
            d.opened,
            vec![ApiVersion(13, 0), ApiVersion(12, 2), ApiVersion(12, 1)]
        );
        assert_eq!(d.destroyed, vec![ApiVersion(13, 0), ApiVersion(12, 2)]);
    });
    drop(encoder);
    assert_clean();
    reset();
    let av1 = session(
        &Negotiated {
            codec: 2,
            hdr: true,
            ..Default::default()
        },
        ApiVersion(12, 1),
    )?;
    // SAFETY: The session configured AV1; the union member holds only integers and pointers.
    let format = unsafe { av1.config.encodeCodecConfig.av1Config };
    assert_eq!(format.enableTemporalSVC(), 1);
    assert_eq!(format.reserved4(), 1);
    assert_eq!(format.inputBitDepth, 0);
    assert_eq!(format.outputBitDepth, 0);
    drop(av1);
    assert_clean();
    Ok(())
}
#[test]
fn completed_output_retains_inputs_and_async_event_survives_a_busy_lock() -> Result<()> {
    reset();
    state(|d| {
        d.asynchronous = true;
        d.need_more_input = true;
        d.lock_busy = 1;
    });
    let mut encoder = session(&Negotiated::default(), ApiVersion(11, 0))?;
    let slot = input(&mut encoder, 17)?;
    let presentation = Instant::now() - Duration::from_millis(10);
    encoder.submit(slot, None, true, presentation)?;
    assert!(encoder.poll()?.is_empty());
    assert_eq!(encoder.pending.len(), 1);
    state(|d| assert!(d.mapped.contains(&17)));
    assert!(encoder.submit(slot, None, false, Instant::now()).is_err());
    let result = encoder.poll()?;
    assert_eq!(result.len(), 1);
    assert!(result[0].idr);
    assert!(!result[0].after_invalidation);
    assert_eq!(result[0].bytes, 1u64.to_le_bytes());
    assert!(result[0].latency.is_some());
    assert_eq!(result[0].presentation, Some(presentation));
    state(|d| assert!(d.mapped.is_empty()));
    encoder.submit(slot, None, false, Instant::now())?;
    assert_eq!(encoder.poll()?[0].bytes, 2u64.to_le_bytes());
    drop(encoder);
    assert_clean();
    state(|d| {
        let position = |name| d.calls.iter().rposition(|v| *v == name).unwrap();
        assert!(position("unlock output") < position("unmap input"));
        assert!(position("unmap input") < position("unregister event"));
        assert!(position("destroy output") < position("unregister input"));
        assert!(position("unregister input") < position("destroy session"));
    });
    Ok(())
}
#[test]
fn timed_out_submission_retains_resources_until_native_session_destruction() -> Result<()> {
    reset();
    state(|d| {
        d.asynchronous = true;
        d.lock_busy = usize::MAX;
        d.destroy_busy = true;
    });
    let mut encoder = session(&Negotiated::default(), ApiVersion(13, 0))?;
    let slot = input(&mut encoder, 17)?;
    encoder.submit(slot, None, true, Instant::now())?;
    encoder.pending.front_mut().unwrap().started =
        Instant::now() - COMPLETION_TIMEOUT - Duration::from_millis(1);
    assert!(encoder.poll().is_err());
    drop(encoder);
    assert_clean();
    Ok(())
}
#[test]
fn partial_output_allocation_is_released_and_reference_caps_control_advertisement() -> Result<()> {
    reset();
    state(|d| d.reject_output = true);
    let mut encoder = session(&Negotiated::default(), ApiVersion(13, 0))?;
    assert!(input(&mut encoder, 17).is_err());
    drop(encoder);
    assert_clean();
    reset();
    state(|d| d.no_multiple_refs = true);
    let encoder = session(&Negotiated::default(), ApiVersion(13, 0))?;
    assert_eq!(encoder.retained, 1);
    assert!(!encoder.invalidation);
    drop(encoder);
    assert_clean();
    Ok(())
}
#[test]
fn loss_feedback_drains_pending_frames_and_only_confirms_successful_invalidation() -> Result<()> {
    reset();
    let mut encoder = session(&Negotiated::default(), ApiVersion(13, 0))?;
    let slot = input(&mut encoder, 17)?;
    for frame in 1..=12 {
        encoder.submit(slot, None, frame == 1, Instant::now())?;
        encoder.poll()?;
    }
    // Feedback can arrive while a later picture is still pending. Drain into
    // the output queue; the feedback range includes all dependent pictures.
    encoder.submit(slot, None, false, Instant::now())?;
    assert!(encoder.invalidate(11, 11)?);
    state(|d| assert_eq!(d.invalidated, vec![11, 12, 13]));
    assert_eq!(encoder.poll()?[0].bytes, 13u64.to_le_bytes());
    encoder.submit(slot, None, false, Instant::now())?;
    assert!(encoder.poll()?[0].after_invalidation);
    encoder.submit(slot, None, false, Instant::now())?;
    assert!(!encoder.poll()?[0].after_invalidation);
    assert!(encoder.invalidate(11, 12)?);
    state(|d| assert_eq!(d.invalidated.len(), 3));
    state(|d| d.reject_invalidate = Some(15));
    assert!(encoder.invalidate(14, 14).is_err());
    encoder.submit(slot, None, false, Instant::now())?;
    assert!(!encoder.poll()?[0].after_invalidation);
    encoder.submit(slot, None, true, Instant::now())?;
    assert!(encoder.poll()?[0].idr);
    assert!(encoder.invalidate(14, 15)?); // Old GOP feedback is already resolved.
    assert!(!encoder.invalidate(17, 17)?); // A lost current IDR needs another IDR.
    drop(encoder);
    assert_clean();
    Ok(())
}
#[test]
fn failed_bitrate_reconfigure_keeps_previous_config_then_higher_rate_requests_idr() -> Result<()> {
    reset();
    let stream = Negotiated {
        codec: 1,
        hdr: true,
        ..Default::default()
    };
    let mut encoder = session(&stream, ApiVersion(13, 0))?;
    let before = encoder.config.rcParams.averageBitRate;
    state(|d| d.reject_reconfigure = true);
    assert!(encoder.bitrate(40000).is_err());
    assert_eq!(encoder.config.rcParams.averageBitRate, before);
    assert_eq!(encoder.stream.bitrate_kbps, 20000);
    state(|d| d.reject_reconfigure = false);
    encoder.bitrate(40000)?;
    assert_eq!(encoder.config.rcParams.averageBitRate, 40_000_000);
    assert_eq!(
        encoder.initialize.encodeConfig,
        encoder.config.as_mut() as *mut _
    );
    state(|d| {
        let (parameters, config) = d.reconfigured.as_ref().unwrap();
        assert_eq!(parameters.forceIDR(), 1);
        assert_eq!(parameters.resetEncoder(), 1);
        assert_eq!(config.rcParams.maxBitRate, 40_000_000);
        assert_eq!(
            // SAFETY: HEVC was configured; the union member is plain integers and pointers.
            unsafe { config.encodeCodecConfig.hevcConfig }
                .hevcVUIParameters
                .transferCharacteristics,
            16
        );
    });
    assert!(encoder.force_idr);
    let slot = input(&mut encoder, 17)?;
    encoder.submit(slot, None, false, Instant::now())?;
    assert!(encoder.poll()?[0].idr);
    encoder.bitrate(10000)?;
    assert!(!encoder.force_idr);
    assert!(encoder.bitrate(0).is_err());
    assert!(encoder.bitrate(800001).is_err());
    drop(encoder);
    assert_clean();
    Ok(())
}
#[test]
fn hdr_metadata_is_snapshotted_until_the_gpu_submission_completes() -> Result<()> {
    reset();
    state(|d| d.asynchronous = true);
    let mut encoder = session(
        &Negotiated {
            codec: 1,
            hdr: true,
            ..Default::default()
        },
        ApiVersion(13, 0),
    )?;
    let slot = input(&mut encoder, 17)?;
    encoder
        .metadata
        .update(butterpollo_core::hdr::Metadata::display(600., 0.002, 300.));
    encoder.submit(slot, None, true, Instant::now())?;
    encoder
        .metadata
        .update(butterpollo_core::hdr::Metadata::display(1400., 0.001, 800.));
    assert_eq!(encoder.slots[slot].metadata.mastering.maxLuma, 6_000_000);
    assert_eq!(encoder.poll()?.len(), 1);
    encoder.submit(slot, None, false, Instant::now())?;
    assert_eq!(encoder.slots[slot].metadata.mastering.maxLuma, 14_000_000);
    assert_eq!(encoder.poll()?.len(), 1);
    drop(encoder);
    assert_clean();
    Ok(())
}

#[test]
#[ignore = "requires explicitly selected NVIDIA hardware and independent FFmpeg decoding"]
fn native_nvenc_loss_recovery_and_444_hdr_decode() -> Result<()> {
    use std::os::windows::process::CommandExt;
    if std::env::var("BUTTERPOLLO_TEST_NVENC").as_deref() != Ok("1") {
        bail!("set BUTTERPOLLO_TEST_NVENC=1 on an NVIDIA test machine");
    }
    let decoder =
        std::env::var_os("BUTTERPOLLO_TEST_FFMPEG").context("set BUTTERPOLLO_TEST_FFMPEG")?;
    let report = std::path::PathBuf::from(
        std::env::var_os("BUTTERPOLLO_TEST_NVENC_REPORT")
            .context("set BUTTERPOLLO_TEST_NVENC_REPORT")?,
    );
    let directory = report
        .parent()
        .context("report requires a parent directory")?;
    std::fs::create_dir_all(directory)?;
    let _com = crate::capture::ComGuard::new()?;
    let gpu = Device::new(&std::env::var("BUTTERPOLLO_TEST_NVENC_DISPLAY").unwrap_or_default())?;
    if !gpu.display.adapter.to_ascii_lowercase().contains("nvidia") {
        bail!("select an NVIDIA-attached output with BUTTERPOLLO_TEST_NVENC_DISPLAY");
    }
    let cases = std::env::var("BUTTERPOLLO_TEST_NVENC_CASES")
        .unwrap_or_else(|_| "h264,hevc-hdr,hevc444-sdr,hevc444-hdr".into());
    let mut reports = Vec::new();
    for name in cases.split(',').map(str::trim) {
        let (codec, hdr, yuv444) = match name {
            "h264" => (0, false, false),
            "hevc-hdr" => (1, true, false),
            "hevc444-sdr" => (1, false, true),
            "hevc444-hdr" => (1, true, true),
            "av1-hdr" => (2, true, false),
            "av1444-hdr" => (2, true, true),
            _ => bail!("unknown NVIDIA test case {name}"),
        };
        let config = Negotiated {
            width: 640,
            height: 480,
            codec,
            hdr,
            yuv444,
            bitrate_kbps: 20000,
            ..Default::default()
        };
        let mut encoder = Encoder::new_device_options(&config, gpu.clone(), &Config::default())?;
        let supported = encoder.supports_invalidation();
        let mut packets = Vec::new();
        for frame in 1..=64 {
            if supported && matches!(frame, 7 | 19) {
                assert!(encoder.invalidate_ref_frames(frame - 2, frame - 1));
            }
            let stride = if hdr { 640 * 8 } else { 640 * 4 };
            let mut image = Image {
                width: 640,
                height: 480,
                stride,
                bytes: vec![0; stride * 480],
                pixel: if hdr { Pixel::RgbaF16 } else { Pixel::Bgra8 },
                captured: Instant::now(),
            };
            for (index, pixel) in image
                .bytes
                .chunks_exact_mut(if hdr { 8 } else { 4 })
                .enumerate()
            {
                if hdr {
                    for channel in 0..3 {
                        let value =
                            ((index / 640 + frame as usize * 3 + channel * 11) % 127) as f32 / 10.;
                        pixel[channel * 2..channel * 2 + 2]
                            .copy_from_slice(&half::f16::from_f32(value).to_bits().to_le_bytes());
                    }
                    pixel[6..].copy_from_slice(&half::f16::ONE.to_bits().to_le_bytes());
                } else {
                    pixel.copy_from_slice(&[
                        (index / 640) as u8,
                        (index + frame as usize * 3) as u8,
                        frame as u8,
                        255,
                    ]);
                }
            }
            packets.extend(encoder.encode(
                &image,
                frame == 1,
                if frame >= 33 { 25000 } else { 20000 },
            )?);
            let deadline = Instant::now() + Duration::from_secs(2);
            while encoder.pending() {
                packets.extend(encoder.poll()?);
                if Instant::now() >= deadline {
                    bail!("NVIDIA output fixture timed out");
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(packets.len(), frame as usize);
        }
        if supported {
            assert!(packets[6].after_invalidation);
            assert!(packets[18].after_invalidation);
        }
        let format = match codec {
            0 => "h264",
            1 => "hevc",
            _ => "obu",
        };
        let path = directory.join(format!("nvenc-{}-{name}.{format}", std::process::id()));
        let bytes: Vec<_> = packets
            .iter()
            .enumerate()
            .filter(|(index, _)| !supported || !matches!(*index, 4 | 5 | 16 | 17))
            .flat_map(|(_, packet)| packet.bytes.iter().copied())
            .collect();
        std::fs::write(&path, bytes)?;
        let ffprobe = std::env::var_os("BUTTERPOLLO_TEST_FFPROBE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(&decoder).with_file_name("ffprobe.exe"));
        let probe = std::process::Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,width,height,pix_fmt,color_transfer,color_primaries,color_space",
                "-of",
                "json",
            ])
            .arg(&path)
            .creation_flags(0x08000000)
            .output()?;
        assert!(probe.status.success() && probe.stderr.is_empty());
        let info: serde_json::Value = serde_json::from_slice(&probe.stdout)?;
        let stream = &info["streams"][0];
        assert_eq!(stream["width"], 640);
        assert_eq!(stream["height"], 480);
        assert_eq!(
            stream["codec_name"],
            match codec {
                0 => "h264",
                1 => "hevc",
                _ => "av1",
            }
        );
        assert_eq!(
            stream["pix_fmt"],
            match (yuv444, hdr) {
                (false, false) => "yuv420p",
                (false, true) => "yuv420p10le",
                (true, false) => "yuv444p",
                (true, true) => "yuv444p10le",
            }
        );
        if hdr {
            assert_eq!(stream["color_transfer"], "smpte2084");
            assert_eq!(stream["color_primaries"], "bt2020");
            assert_eq!(stream["color_space"], "bt2020nc");
        }
        let output = std::process::Command::new(&decoder)
            .args([
                "-hide_banner",
                "-nostdin",
                "-v",
                "error",
                "-xerror",
                "-err_detect",
                "explode",
                "-f",
                format,
                "-i",
            ])
            .arg(&path)
            .args([
                "-an",
                "-progress",
                "pipe:1",
                "-nostats",
                "-fps_mode",
                "passthrough",
                "-f",
                "null",
                "-",
            ])
            .creation_flags(0x08000000)
            .output()?;
        assert!(
            output.status.success() && output.stderr.is_empty(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let decoded = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("frame=")?.trim().parse::<u32>().ok())
            .next_back()
            .context("decoder reported no frames")?;
        assert_eq!(decoded, if supported { 60 } else { 64 });
        reports.push(serde_json::json!({"case":name,"codec":codec,"hdr":hdr,"yuv444":yuv444,"decoded":decoded,
            "native_rfi":supported,"recovery_frames":packets.iter().filter(|p|p.after_invalidation).count(),"bitrate_reconfigured":true}));
    }
    std::fs::write(report, serde_json::to_vec_pretty(&reports)?)?;
    Ok(())
}
