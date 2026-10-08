//! Endpoint routing is shared by stream owners and restored after the last
//! owner, including host crashes. Capture-only sinks never alter defaults.
#![warn(clippy::undocumented_unsafe_blocks)]

use crate::text::to_wide;
use anyhow::{Context, Result, bail};
use butterpollo_core::{config::Config, session::AudioPreparation};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::c_void,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use windows::{
    Win32::{
        Foundation::PROPERTYKEY,
        Media::Audio::*,
        System::Com::{StructuredStorage::*, *},
    },
    core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PCWSTR},
};

const ROLES: [ERole; 3] = [eConsole, eMultimedia, eCommunications];
const FRIENDLY: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};
/// PKEY_Device_DeviceDesc: the endpoint's own name, e.g. "Speakers".
const DESCRIPTION: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 2,
};
/// PKEY_DeviceInterface_FriendlyName: the adapter, e.g. "Steam Streaming Speakers".
const ADAPTER: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x026e516e_b814_414b_83cd_856d6fef4822),
    pid: 2,
};
#[derive(Clone, Serialize)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub description: String,
    pub adapter: String,
    pub default: bool,
    pub virtual_sink: bool,
}
unsafe fn device_id(device: &IMMDevice) -> Result<String> {
    // SAFETY: `device` is a live IMMDevice borrowed for the call.
    let id = unsafe { device.GetId()? };
    // SAFETY: GetId returned a NUL-terminated string that is not freed until below.
    let value = unsafe { id.to_string() };
    // SAFETY: `id` came from CoTaskMemAlloc in GetId and is freed once, after its last use.
    unsafe {
        CoTaskMemFree(Some(id.0.cast()));
    }
    Ok(value?)
}
fn enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: CoCreateInstance only reads the static CLSID, and fails cleanly when COM is not
    // initialised.
    Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? })
}
pub fn endpoints() -> Result<Vec<Endpoint>> {
    let _com = crate::capture::ComGuard::new()?;
    let enumerator = enumerator()?;
    // SAFETY: COM is initialised on this thread by `_com`, every interface used is live, and each
    // string from PropVariantToStringAlloc is freed once after it is copied.
    unsafe {
        let default = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .ok()
            .and_then(|d| device_id(&d).ok());
        let list = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let mut endpoints = Vec::new();
        for index in 0..list.GetCount()? {
            let device = list.Item(index)?;
            let id = device_id(&device)?;
            let store = device.OpenPropertyStore(STGM_READ).ok();
            let text = |key: &PROPERTYKEY| {
                store.as_ref().and_then(|store| {
                    let mut property = store.GetValue(key).ok()?;
                    let text = PropVariantToStringAlloc(&property).ok().and_then(|text| {
                        let result = text.to_string().ok();
                        CoTaskMemFree(Some(text.0.cast()));
                        result
                    });
                    let _ = PropVariantClear(&mut property);
                    text
                })
            };
            let name = text(&FRIENDLY).unwrap_or_else(|| id.clone());
            let description = text(&DESCRIPTION).unwrap_or_default();
            let adapter = text(&ADAPTER).unwrap_or_default();
            let virtual_sink = [&name, &adapter]
                .iter()
                .any(|n| n.to_ascii_lowercase().contains("steam streaming speakers"));
            endpoints.push(Endpoint {
                default: default.as_ref() == Some(&id),
                id,
                name,
                description,
                adapter,
                virtual_sink,
            });
        }
        Ok(endpoints)
    }
}

// PolicyConfig is the Windows interface used by the retained host. Keep its
// actual COM slot layout, including the unused methods before SetDefault.
#[repr(transparent)]
#[derive(Clone)]
struct Policy(IUnknown);
#[repr(C)]
struct PolicyVtbl {
    unknown: IUnknown_Vtbl,
    mix: usize,
    device_format:
        unsafe extern "system" fn(*mut c_void, PCWSTR, i32, *mut *mut WAVEFORMATEX) -> HRESULT,
    reset_format: usize,
    set_format: unsafe extern "system" fn(
        *mut c_void,
        PCWSTR,
        *const WAVEFORMATEX,
        *const WAVEFORMATEX,
    ) -> HRESULT,
    unused: [usize; 6],
    default: unsafe extern "system" fn(*mut c_void, PCWSTR, ERole) -> HRESULT,
    visibility: usize,
}
// SAFETY: PolicyVtbl mirrors IPolicyConfig's slot layout (the offset test checks it) and Policy is
// a transparent IUnknown, so the IID names an interface with exactly this vtable.
unsafe impl Interface for Policy {
    type Vtable = PolicyVtbl;
    const IID: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
}
impl Policy {
    fn new() -> Result<Self> {
        // SAFETY: CoCreateInstance only reads the static CLSID and returns an owned interface or an
        // error.
        Ok(unsafe {
            CoCreateInstance(
                &GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9),
                None,
                CLSCTX_ALL,
            )?
        })
    }
    fn set_default(&self, id: &str, role: ERole) -> Result<()> {
        let id = to_wide(id);
        // SAFETY: `self` is a live IPolicyConfig whose slot matches PolicyVtbl, and `id` is
        // NUL-terminated and outlives the call.
        unsafe {
            (self.vtable().default)(self.as_raw(), PCWSTR(id.as_ptr()), role).ok()?;
        }
        Ok(())
    }
    fn format(&self, id: &str) -> Result<Vec<u8>> {
        let id = to_wide(id);
        let mut format = std::ptr::null_mut();
        // SAFETY: `format` receives a CoTaskMemAlloc'd WAVEFORMATEX that is read only within its
        // header plus cbSize bytes and freed once.
        unsafe {
            (self.vtable().device_format)(self.as_raw(), PCWSTR(id.as_ptr()), 0, &mut format)
                .ok()?;
            if format.is_null() {
                bail!("audio endpoint returned no format");
            }
            let size = size_of::<WAVEFORMATEX>() + usize::from((*format).cbSize);
            let result = if size > 4096 {
                Err(anyhow::anyhow!("invalid audio endpoint format"))
            } else {
                Ok(std::slice::from_raw_parts(format.cast::<u8>(), size).to_vec())
            };
            CoTaskMemFree(Some(format.cast()));
            result
        }
    }
    fn set_format(&self, id: &str, bytes: &[u8]) -> Result<()> {
        if bytes.len() < size_of::<WAVEFORMATEX>() || bytes.len() > 4096 {
            bail!("invalid saved audio format");
        }
        let id = to_wide(id);
        // Stored format bytes need native alignment when passed back to COM.
        let mut storage = vec![0u64; bytes.len().div_ceil(8)];
        // SAFETY: `storage` is u64-aligned and at least `bytes.len()` long, and `id` and `empty`
        // outlive the call, whose slot matches PolicyVtbl.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), storage.as_mut_ptr().cast(), bytes.len());
            let empty = WAVEFORMATEXTENSIBLE::default();
            (self.vtable().set_format)(
                self.as_raw(),
                PCWSTR(id.as_ptr()),
                storage.as_ptr().cast(),
                (&empty as *const WAVEFORMATEXTENSIBLE).cast(),
            )
            .ok()?;
        }
        Ok(())
    }
}
fn defaults() -> Result<[Option<String>; 3]> {
    let enumerator = enumerator()?;
    // SAFETY: `enumerator` is a live interface and device_id only needs a live IMMDevice.
    Ok(ROLES.map(|role| unsafe {
        enumerator
            .GetDefaultAudioEndpoint(eRender, role)
            .ok()
            .and_then(|d| device_id(&d).ok())
    }))
}
#[derive(Clone, Serialize, Deserialize)]
struct FormatChange {
    id: String,
    before: Vec<u8>,
    applied: Vec<u8>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Journal {
    before: [Option<String>; 3],
    applied: String,
    format: Option<FormatChange>,
}
struct Active {
    directory: PathBuf,
    sink: String,
    virtual_sink: bool,
    users: usize,
    /// The channels each stream asked for; the sink gets the most.
    channels: BTreeMap<u64, usize>,
}
/// The speaker layout for streams sharing the sink: the most channels any of
/// them asked for. Capture mixes down for the others; following the last
/// stream instead made two clients reformat the sink in turn, and each
/// reformat interrupted the other's audio.
fn shared_channels(requests: &BTreeMap<u64, usize>) -> Option<usize> {
    requests.values().copied().max()
}
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
pub struct Route {
    pub sink: String,
    directory: PathBuf,
    keep_default: bool,
    capture_only: bool,
    virtual_sink: bool,
    /// This stream's place among the users of the shared sink.
    id: u64,
}
static NEXT_ROUTE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn path(directory: &Path) -> PathBuf {
    directory.join("audio-recovery.json")
}
fn save(directory: &Path, journal: &Journal) -> Result<()> {
    butterpollo_core::state::write_json(&path(directory), journal)
}
fn restore(directory: &Path) -> Result<()> {
    let file = path(directory);
    if !file.exists() {
        return Ok(());
    }
    let _com = crate::capture::ComGuard::new()?;
    let journal: Journal = serde_json::from_slice(&std::fs::read(&file)?)?;
    if journal.applied.is_empty() && journal.format.is_none() {
        return Ok(());
    }
    let policy = Policy::new()?;
    let current = defaults()?;
    // Every role and the format are tried: one device that is unavailable
    // (Bluetooth off, a TV's audio not back yet) must not leave the others.
    // On failure the journal stays; a retry only changes roles still on the
    // endpoint this host set.
    let mut failed = None;
    for (index, role) in ROLES.into_iter().enumerate() {
        if current[index].as_deref() == Some(&journal.applied)
            && let Some(before) = &journal.before[index]
            && let Err(error) = policy.set_default(before, role)
        {
            failed.get_or_insert(error.context("restoring the previous audio endpoint"));
        }
    }
    if let Some(format) = &journal.format {
        match policy.format(&format.id) {
            Ok(current) if current == format.applied => {
                if let Err(error) = policy.set_format(&format.id, &format.before) {
                    failed.get_or_insert(error);
                }
            }
            Ok(_) => {}
            Err(error) => {
                failed.get_or_insert(error);
            }
        }
    }
    if let Some(error) = failed {
        return Err(error);
    }
    save(directory, &Journal::default())
}
/// The journal of a restore that has not completed yet, if any.
fn pending(directory: &Path) -> Option<Journal> {
    let journal: Journal = serde_json::from_slice(&std::fs::read(path(directory)).ok()?).ok()?;
    (!journal.applied.is_empty() || journal.format.is_some()).then_some(journal)
}
/// The defaults to restore after this stream. When a restore is still
/// pending (the speakers were off when the last stream ended), a role that
/// is still on the endpoint this host set keeps the original from then: the
/// current default would be the virtual sink, and the user's speakers would
/// be lost for good.
fn originals(current: [Option<String>; 3], pending: Option<&Journal>) -> [Option<String>; 3] {
    let Some(pending) = pending.filter(|journal| !journal.applied.is_empty()) else {
        return current;
    };
    std::array::from_fn(|index| match &pending.before[index] {
        Some(original) if current[index].as_deref() == Some(pending.applied.as_str()) => {
            Some(original.clone())
        }
        _ => current[index].clone(),
    })
}
pub fn recover(directory: &Path) -> Result<()> {
    let active = ACTIVE
        .try_lock()
        .map_err(|_| anyhow::anyhow!("audio routing is busy"))?;
    if active.is_some() {
        bail!("audio routing is in use");
    }
    restore(directory)
}
/// An endpoint by ID, friendly name, description or adapter name, in that
/// order, as Vibepollo matches audio_sink and virtual_sink.
fn find<'a>(endpoints: &'a [Endpoint], requested: &str) -> Option<&'a Endpoint> {
    let requested = requested.trim();
    let fields: [fn(&Endpoint) -> &str; 4] =
        [|e| &e.id, |e| &e.name, |e| &e.description, |e| &e.adapter];
    fields.iter().find_map(|field| {
        endpoints
            .iter()
            .find(|endpoint| field(endpoint).eq_ignore_ascii_case(requested))
    })
}
/// Install Steam Streaming Speakers when Steam provides them and none exist.
/// Best effort: streaming continues on another endpoint if this fails.
fn install_steam(config: &Config, endpoints: &[Endpoint]) -> bool {
    match try_install_steam(config, endpoints) {
        Ok(installed) => installed,
        Err(error) => {
            tracing::warn!(error = %format!("{error:#}"), "Steam Streaming Speakers could not be installed");
            false
        }
    }
}
fn try_install_steam(config: &Config, endpoints: &[Endpoint]) -> Result<bool> {
    if endpoints.iter().any(|endpoint| endpoint.virtual_sink)
        || !config.boolean("install_steam_audio_drivers", true)
    {
        return Ok(false);
    }
    let Some(directory) = std::env::var_os("CommonProgramFiles(x86)") else {
        return Ok(false);
    };
    let inf =
        PathBuf::from(directory).join("Steam/drivers/Windows10/x64/SteamStreamingSpeakers.inf");
    if !inf.is_file() {
        return Ok(false);
    }
    let file = to_wide(&inf.to_string_lossy());
    // SAFETY: `file` is NUL-terminated and outlives the call.
    unsafe {
        use windows::Win32::Devices::DeviceAndDriverInstallation::{
            DIIRFLAG_FORCE_INF, DiInstallDriverW,
        };
        DiInstallDriverW(None, PCWSTR(file.as_ptr()), DIIRFLAG_FORCE_INF, None)?;
    }
    Ok(true)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sample {
    Float32,
    Pcm32,
    Pcm24In32,
    Pcm24,
    Pcm16,
}
impl Sample {
    fn bits(self) -> (u16, u16) {
        match self {
            Self::Float32 | Self::Pcm32 => (32, 32),
            Self::Pcm24In32 => (32, 24),
            Self::Pcm24 => (24, 24),
            Self::Pcm16 => (16, 16),
        }
    }
}
fn virtual_format(channels: usize, sample: Sample, side: bool) -> Vec<u8> {
    let (bits, valid_bits) = sample.bits();
    let format = WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: 65534,
            nChannels: channels as u16,
            nSamplesPerSec: 48000,
            nAvgBytesPerSec: 48000 * channels as u32 * u32::from(bits / 8),
            nBlockAlign: channels as u16 * (bits / 8),
            wBitsPerSample: bits,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: valid_bits,
        },
        dwChannelMask: match channels {
            6 if side => 0x60f,
            6 => 0x3f,
            8 => 0x63f,
            _ => 3,
        },
        SubFormat: GUID::from_u128(if sample == Sample::Float32 {
            0x00000003_0000_0010_8000_00aa00389b71
        } else {
            0x00000001_0000_0010_8000_00aa00389b71
        }),
    };
    // SAFETY: `format` is a live packed WAVEFORMATEXTENSIBLE with no padding, so all its bytes are
    // initialised.
    unsafe {
        std::slice::from_raw_parts(
            (&format as *const WAVEFORMATEXTENSIBLE).cast(),
            size_of::<WAVEFORMATEXTENSIBLE>(),
        )
        .to_vec()
    }
}
fn valid_bits(bytes: &[u8]) -> u16 {
    // Policy::format has already checked the native format's allocation size.
    // SAFETY: callers pass bytes from Policy::format or virtual_format, which hold at least a whole
    // WAVEFORMATEX, and the read is unaligned.
    let format = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<WAVEFORMATEX>()) };
    if format.wFormatTag == 65534 && bytes.len() >= size_of::<WAVEFORMATEXTENSIBLE>() {
        // SAFETY: `bytes` was just checked to hold a whole WAVEFORMATEXTENSIBLE, and the read is
        // unaligned.
        let extended =
            unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<WAVEFORMATEXTENSIBLE>()) };
        // SAFETY: both fields of the union are u16, so any bits are a valid value.
        let bits = unsafe { extended.Samples.wValidBitsPerSample };
        if bits > 0 && bits <= format.wBitsPerSample {
            return bits;
        }
    }
    format.wBitsPerSample
}
fn virtual_formats(channels: usize, preferred_bits: u16) -> Result<Vec<Vec<u8>>> {
    if !matches!(channels, 2 | 6 | 8) {
        bail!("virtual audio requires stereo, 5.1 or 7.1");
    }
    let samples = if channels == 2 {
        // Retain the old host's stereo preference, including spatial audio.
        [
            Sample::Pcm24In32,
            Sample::Pcm24,
            Sample::Pcm16,
            Sample::Float32,
            Sample::Pcm32,
        ]
    } else {
        [
            Sample::Float32,
            Sample::Pcm32,
            Sample::Pcm24In32,
            Sample::Pcm24,
            Sample::Pcm16,
        ]
    };
    let mut output = Vec::new();
    // Match the playback device's valid depth first. A rejected format must
    // not prevent capturing an otherwise usable virtual speaker endpoint.
    for preferred in [true, false] {
        for sample in samples {
            if (sample.bits().1 == preferred_bits) != preferred {
                continue;
            }
            output.push(virtual_format(channels, sample, false));
            if channels == 6 {
                output.push(virtual_format(channels, sample, true));
            }
        }
    }
    Ok(output)
}
fn apply_virtual_format(
    policy: &Policy,
    id: &str,
    channels: usize,
    bits: u16,
    directory: &Path,
    journal: &mut Journal,
) -> Result<()> {
    let mut last_error = None;
    for desired in virtual_formats(channels, bits)? {
        journal
            .format
            .as_mut()
            .context("virtual audio format is not owned")?
            .applied = desired.clone();
        save(directory, journal)?;
        crate::display_recovery::audio(true)?;
        match policy.set_format(id, &desired) {
            Ok(()) => {
                journal.format.as_mut().unwrap().applied = policy.format(id)?;
                save(directory, journal)?;
                return Ok(());
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.context("no usable virtual audio format")?)
        .with_context(|| format!("setting {channels}-channel virtual speaker format"))
}
fn virtual_sink_warning(
    host_audio: bool,
    capture_only: bool,
    virtual_sink: bool,
) -> Option<&'static str> {
    (!host_audio && !capture_only && !virtual_sink).then_some("Virtual audio sink unavailable; capturing a physical playback device. Sound may play on the host and surround channels may be lost. Select an installed virtual sink in Audio settings or enable host audio intentionally.")
}
impl AudioPreparation<Route> for Arc<Route> {
    fn route(&self) -> Arc<Route> {
        self.clone()
    }
}
impl Route {
    pub fn warning(&self, host_audio: bool) -> Option<&'static str> {
        virtual_sink_warning(host_audio, self.capture_only, self.virtual_sink)
    }
    pub fn acquire(
        config: &Config,
        directory: &Path,
        host_audio: bool,
        channels: usize,
    ) -> Result<Self> {
        let _com = crate::capture::ComGuard::new()?;
        let mut active = ACTIVE.lock().unwrap();
        let capture_only = config.boolean("audio_sink_capture_only", false)
            && !config.get("audio_sink", "").is_empty()
            && config.get("virtual_sink", "").is_empty();
        if let Some(shared) = active.as_mut() {
            if shared.directory != directory {
                bail!("audio is owned by another host configuration");
            }
            shared.users += 1;
            return Ok(Self {
                sink: shared.sink.clone(),
                directory: directory.into(),
                keep_default: config.boolean("keep_sink_default", true),
                capture_only,
                virtual_sink: shared.virtual_sink,
                id: NEXT_ROUTE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            });
        }
        let mut available = endpoints()?;
        // Taken before any driver installation, which can move the defaults.
        let pending = pending(directory);
        let before = originals(defaults()?, pending.as_ref());
        if !host_audio && !capture_only && install_steam(config, &available) {
            // The new endpoint appears shortly after installation.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                available = endpoints()?;
                if available.iter().any(|endpoint| endpoint.virtual_sink)
                    || std::time::Instant::now() >= deadline
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            let moved = defaults()?;
            if moved != before
                && let Ok(policy) = Policy::new()
            {
                for (index, role) in ROLES.into_iter().enumerate() {
                    if let Some(previous) = &before[index]
                        && moved[index].as_ref() != Some(previous)
                    {
                        let _ = policy.set_default(previous, role);
                    }
                }
                available = endpoints()?;
            }
        }
        let configured = config.get("virtual_sink", "");
        let configured = if configured.is_empty() {
            config.get("audio_sink", "")
        } else {
            configured
        };
        let default = available
            .iter()
            .find(|endpoint| endpoint.default)
            .context("no default playback endpoint")?;
        let selected = if capture_only {
            find(&available, configured)
                .context("configured capture-only audio sink is unavailable")?
        } else if !host_audio || !config.get("virtual_sink", "").is_empty() {
            if !config.get("virtual_sink", "").is_empty() {
                find(&available, configured)
                    .context("configured virtual audio sink is unavailable")?
            } else {
                available
                    .iter()
                    .find(|endpoint| endpoint.virtual_sink)
                    .or_else(|| find(&available, configured))
                    .unwrap_or(default)
            }
        } else if configured.is_empty() {
            default
        } else {
            find(&available, configured).context("configured audio sink is unavailable")?
        };
        let mut journal = Journal {
            before,
            applied: if capture_only {
                String::new()
            } else {
                selected.id.clone()
            },
            format: None,
        };
        let managed_virtual = selected.virtual_sink || !config.get("virtual_sink", "").is_empty();
        if let Some(message) = virtual_sink_warning(host_audio, capture_only, managed_virtual) {
            tracing::warn!("{message}");
        }
        let policy = if capture_only {
            None
        } else {
            Some(Policy::new()?)
        };
        let result = (|| -> Result<()> {
            if managed_virtual && !capture_only {
                let policy = policy.as_ref().unwrap();
                let current = policy.format(&selected.id)?;
                // A pending restore's original format wins over the one this
                // host left, as for the default devices.
                let before = match pending.as_ref().and_then(|j| j.format.as_ref()) {
                    Some(format) if format.id == selected.id && format.applied == current => {
                        format.before.clone()
                    }
                    _ => current,
                };
                let default_format = policy.format(&default.id)?;
                let bits = valid_bits(&default_format);
                journal.format = Some(FormatChange {
                    id: selected.id.clone(),
                    before,
                    applied: Vec::new(),
                });
                apply_virtual_format(
                    policy,
                    &selected.id,
                    channels,
                    bits,
                    directory,
                    &mut journal,
                )?;
            }
            if !capture_only
                && journal
                    .before
                    .iter()
                    .any(|id| id.as_deref() != Some(&selected.id))
            {
                save(directory, &journal)?;
                crate::display_recovery::audio(true)?;
                for role in ROLES {
                    policy.as_ref().unwrap().set_default(&selected.id, role)?;
                }
            }
            save(directory, &journal)?;
            Ok(())
        })();
        if let Err(error) = result {
            if restore(directory).is_ok() {
                let _ = crate::display_recovery::audio(false);
            }
            return Err(error);
        }
        let id = NEXT_ROUTE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *active = Some(Active {
            directory: directory.into(),
            sink: selected.id.clone(),
            virtual_sink: managed_virtual,
            users: 1,
            channels: BTreeMap::from([(id, channels)]),
        });
        Ok(Self {
            sink: selected.id.clone(),
            directory: directory.into(),
            keep_default: config.boolean("keep_sink_default", true),
            capture_only,
            virtual_sink: managed_virtual,
            id,
        })
    }
    /// Re-pin managed virtual speakers when keep_sink_default is enabled. Save
    /// the user's newly selected endpoint so teardown restores that choice.
    pub fn maintain_default(&self) -> Result<()> {
        let _active = ACTIVE.lock().unwrap();
        if !self.keep_default || self.capture_only || !self.virtual_sink {
            return Ok(());
        }
        let current = defaults()?;
        if current.iter().all(|id| id.as_deref() == Some(&self.sink)) {
            return Ok(());
        }
        let mut journal: Journal = serde_json::from_slice(&std::fs::read(path(&self.directory))?)?;
        for (index, id) in current.iter().enumerate() {
            if id.as_deref() != Some(&self.sink) {
                journal.before[index] = id.clone();
            }
        }
        save(&self.directory, &journal)?;
        let policy = Policy::new()?;
        for role in ROLES {
            policy.set_default(&self.sink, role)?;
        }
        Ok(())
    }
    pub fn capture_sink(&self, config: &Config) -> Result<String> {
        if self.capture_only || self.virtual_sink || !config.boolean("auto_capture_sink", true) {
            return Ok(self.sink.clone());
        }
        let current = defaults()?;
        Ok(current[0].clone().unwrap_or_else(|| self.sink.clone()))
    }
    pub fn set_channels(&self, channels: usize) -> Result<()> {
        let mut active = ACTIVE.lock().unwrap();
        if !self.virtual_sink || self.capture_only {
            return Ok(());
        }
        let channels = match active.as_mut() {
            Some(shared) => {
                shared.channels.insert(self.id, channels);
                shared_channels(&shared.channels).unwrap_or(channels)
            }
            None => channels,
        };
        let policy = Policy::new()?;
        let current = policy.format(&self.sink)?;
        // SAFETY: `current` came from Policy::format, which returns at least a whole WAVEFORMATEX.
        let format = unsafe { std::ptr::read_unaligned(current.as_ptr().cast::<WAVEFORMATEX>()) };
        if usize::from(format.nChannels) == channels {
            return Ok(());
        }
        let mut journal: Journal = serde_json::from_slice(&std::fs::read(path(&self.directory))?)?;
        if journal.format.is_none() {
            bail!("virtual audio format is not owned");
        }
        apply_virtual_format(
            &policy,
            &self.sink,
            channels,
            valid_bits(&current),
            &self.directory,
            &mut journal,
        )
    }
}
impl Drop for Route {
    fn drop(&mut self) {
        let mut active = ACTIVE.lock().unwrap();
        let Some(shared) = active.as_mut() else {
            return;
        };
        shared.users -= 1;
        // The sink keeps its layout for the streams still on it.
        shared.channels.remove(&self.id);
        if shared.users != 0 {
            return;
        }
        if let Err(error) = restore(&shared.directory) {
            tracing::error!(%error, "audio defaults will be restored by crash recovery");
        } else {
            let _ = crate::display_recovery::audio(false);
        }
        active.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_audio_fallback_warns_only_when_a_virtual_route_was_expected() {
        assert!(
            virtual_sink_warning(false, false, false)
                .unwrap()
                .contains("Sound may play on the host")
        );
        for (host, capture, virtual_sink) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            assert!(virtual_sink_warning(host, capture, virtual_sink).is_none());
        }
    }

    #[test]
    fn streams_sharing_the_sink_get_the_most_channels_any_asked_for() {
        assert_eq!(shared_channels(&BTreeMap::new()), None);
        // A stereo phone and a 5.1 living-room PC on the same app: 5.1 for
        // both, whoever reopened capture last.
        let both = BTreeMap::from([(1, 2), (2, 6)]);
        assert_eq!(shared_channels(&both), Some(6));
        assert_eq!(shared_channels(&BTreeMap::from([(1, 2)])), Some(2));
    }
    #[test]
    fn a_pending_restore_keeps_the_users_original_speakers() {
        let some = |id: &str| Some(id.to_owned());
        let current = [some("virtual"), some("virtual"), some("headset")];
        // Nothing pending: the current defaults are the originals.
        assert_eq!(originals(current.clone(), None), current);
        // The last restore failed while the speakers were off: roles still on
        // the virtual sink keep the speakers; a role the user changed since
        // keeps the user's choice.
        let pending = Journal {
            before: [some("speakers"), some("speakers"), some("speakers")],
            applied: "virtual".into(),
            format: None,
        };
        assert_eq!(
            originals(current.clone(), Some(&pending)),
            [some("speakers"), some("speakers"), some("headset")]
        );
        // A capture-only journal sets no defaults.
        let capture_only = Journal {
            applied: String::new(),
            ..pending
        };
        assert_eq!(originals(current.clone(), Some(&capture_only)), current);
    }
    #[test]
    fn sinks_match_by_id_name_description_or_adapter() {
        let endpoint = |id: &str, name: &str, description: &str, adapter: &str| Endpoint {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            adapter: adapter.into(),
            default: false,
            virtual_sink: false,
        };
        let endpoints = [
            endpoint("{a}", "Speakers (Realtek)", "Speakers", "Realtek(R) Audio"),
            endpoint(
                "{b}",
                "Speakers (Steam Streaming Speakers)",
                "Speakers",
                "Steam Streaming Speakers",
            ),
        ];
        let id = |name: &str| find(&endpoints, name).map(|e| e.id.as_str());
        assert_eq!(id("{B}"), Some("{b}"));
        assert_eq!(id("speakers (realtek)"), Some("{a}"));
        assert_eq!(id("Steam Streaming Speakers"), Some("{b}"));
        // A shared description picks the first endpoint, as Vibepollo does.
        assert_eq!(id("Speakers"), Some("{a}"));
        assert_eq!(id("Headphones"), None);
    }
    #[test]
    fn virtual_speakers_preserve_valid_depth_and_offer_pcm_fallbacks() -> Result<()> {
        let formats = virtual_formats(2, 24)?;
        // SAFETY: `formats[0]` is a whole WAVEFORMATEXTENSIBLE from virtual_format, read unaligned.
        let first =
            unsafe { std::ptr::read_unaligned(formats[0].as_ptr().cast::<WAVEFORMATEXTENSIBLE>()) };
        let container_bits = first.Format.wBitsPerSample;
        let alignment = first.Format.nBlockAlign;
        assert_eq!(container_bits, 32);
        assert_eq!(valid_bits(&formats[0]), 24);
        assert_eq!(alignment, 8);
        let subformat = first.SubFormat;
        assert_eq!(
            subformat,
            GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71)
        );
        let formats = virtual_formats(2, 32)?;
        assert_eq!(formats.len(), 5);
        // SAFETY: `formats[1]` is a whole WAVEFORMATEXTENSIBLE from virtual_format, read unaligned.
        let pcm =
            unsafe { std::ptr::read_unaligned(formats[1].as_ptr().cast::<WAVEFORMATEXTENSIBLE>()) };
        let subformat = pcm.SubFormat;
        assert_eq!(
            subformat,
            GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71)
        );
        assert!(virtual_formats(4, 24).is_err());
        assert_eq!(virtual_formats(6, 16)?.len(), 10);
        assert_eq!(virtual_formats(8, 16)?.len(), 5);
        Ok(())
    }
    #[test]
    fn policy_config_abi_and_surround_formats_match_windows_contract() {
        assert_eq!(
            std::mem::offset_of!(PolicyVtbl, default),
            13 * size_of::<usize>()
        );
        for (channels, mask) in [(2, 3), (6, 0x3f), (8, 0x63f)] {
            let bytes = virtual_format(channels, Sample::Pcm24, false);
            // SAFETY: `bytes` is a whole WAVEFORMATEXTENSIBLE from virtual_format, read unaligned.
            let format =
                unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<WAVEFORMATEXTENSIBLE>()) };
            let actual_mask = format.dwChannelMask;
            let rate = format.Format.nAvgBytesPerSec;
            assert_eq!(actual_mask, mask);
            assert_eq!(rate, 48000 * channels as u32 * 3);
        }
    }
}
