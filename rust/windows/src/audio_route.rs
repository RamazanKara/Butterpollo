//! Endpoint routing is shared by stream owners and restored after the last
//! owner, including host crashes. Capture-only sinks never alter defaults.
use anyhow::{Context, Result, bail};
use butterpollo_core::config::Config;
use serde::{Deserialize, Serialize};
use std::{
    ffi::c_void,
    path::{Path, PathBuf},
    sync::Mutex,
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
#[derive(Clone, Serialize)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub default: bool,
    pub virtual_sink: bool,
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
unsafe fn device_id(device: &IMMDevice) -> Result<String> {
    let id = unsafe { device.GetId()? };
    let value = unsafe { id.to_string() };
    unsafe {
        CoTaskMemFree(Some(id.0.cast()));
    }
    Ok(value?)
}
fn enumerator() -> Result<IMMDeviceEnumerator> {
    Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? })
}
pub fn endpoints() -> Result<Vec<Endpoint>> {
    let _com = crate::capture::ComGuard::new()?;
    let enumerator = enumerator()?;
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
            let name = device
                .OpenPropertyStore(STGM_READ)
                .ok()
                .and_then(|store| {
                    let mut property = store.GetValue(&FRIENDLY).ok()?;
                    let name = PropVariantToStringAlloc(&property).ok().and_then(|name| {
                        let result = name.to_string().ok();
                        CoTaskMemFree(Some(name.0.cast()));
                        result
                    });
                    let _ = PropVariantClear(&mut property);
                    name
                })
                .unwrap_or_else(|| id.clone());
            let virtual_sink = name
                .to_ascii_lowercase()
                .contains("steam streaming speakers");
            endpoints.push(Endpoint {
                default: default.as_ref() == Some(&id),
                id,
                name,
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
unsafe impl Interface for Policy {
    type Vtable = PolicyVtbl;
    const IID: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
}
impl Policy {
    fn new() -> Result<Self> {
        Ok(unsafe {
            CoCreateInstance(
                &GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9),
                None,
                CLSCTX_ALL,
            )?
        })
    }
    fn set_default(&self, id: &str, role: ERole) -> Result<()> {
        let id = wide(id);
        unsafe {
            (self.vtable().default)(self.as_raw(), PCWSTR(id.as_ptr()), role).ok()?;
        }
        Ok(())
    }
    fn format(&self, id: &str) -> Result<Vec<u8>> {
        let id = wide(id);
        let mut format = std::ptr::null_mut();
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
        let id = wide(id);
        // Stored format bytes need native alignment when passed back to COM.
        let mut storage = vec![0u64; bytes.len().div_ceil(8)];
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
}
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
pub struct Route {
    pub sink: String,
    directory: PathBuf,
    keep_default: bool,
    capture_only: bool,
    virtual_sink: bool,
}
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
    for (index, role) in ROLES.into_iter().enumerate() {
        if current[index].as_deref() == Some(&journal.applied)
            && let Some(before) = &journal.before[index]
        {
            policy
                .set_default(before, role)
                .context("restoring the previous audio endpoint")?;
        }
    }
    if let Some(format) = &journal.format
        && policy.format(&format.id)? == format.applied
    {
        policy.set_format(&format.id, &format.before)?;
    }
    save(directory, &Journal::default())
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
fn find<'a>(endpoints: &'a [Endpoint], requested: &str) -> Option<&'a Endpoint> {
    endpoints.iter().find(|endpoint| {
        endpoint.id.eq_ignore_ascii_case(requested) || endpoint.name.eq_ignore_ascii_case(requested)
    })
}
fn install_steam(config: &Config, endpoints: &[Endpoint]) -> Result<bool> {
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
    let file = wide(&inf.to_string_lossy());
    unsafe {
        use windows::Win32::Devices::DeviceAndDriverInstallation::{
            DIIRFLAG_FORCE_INF, DiInstallDriverW,
        };
        DiInstallDriverW(None, PCWSTR(file.as_ptr()), DIIRFLAG_FORCE_INF, None)?;
    }
    Ok(true)
}
fn virtual_format(channels: usize, bits: u16, side: bool) -> Vec<u8> {
    let bits = if matches!(bits, 16 | 24 | 32) {
        bits
    } else {
        32
    };
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
            wValidBitsPerSample: bits,
        },
        dwChannelMask: match channels {
            6 if side => 0x60f,
            6 => 0x3f,
            8 => 0x63f,
            _ => 3,
        },
        SubFormat: GUID::from_u128(if bits == 32 {
            0x00000003_0000_0010_8000_00aa00389b71
        } else {
            0x00000001_0000_0010_8000_00aa00389b71
        }),
    };
    unsafe {
        std::slice::from_raw_parts(
            (&format as *const WAVEFORMATEXTENSIBLE).cast(),
            size_of::<WAVEFORMATEXTENSIBLE>(),
        )
        .to_vec()
    }
}
impl Route {
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
            });
        }
        let mut available = endpoints()?;
        if !host_audio && !capture_only && install_steam(config, &available)? {
            available = endpoints()?;
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
            before: defaults()?,
            applied: if capture_only {
                String::new()
            } else {
                selected.id.clone()
            },
            format: None,
        };
        let managed_virtual = selected.virtual_sink || !config.get("virtual_sink", "").is_empty();
        let policy = if capture_only {
            None
        } else {
            Some(Policy::new()?)
        };
        let result = (|| -> Result<()> {
            if managed_virtual && !capture_only {
                let policy = policy.as_ref().unwrap();
                let before = policy.format(&selected.id)?;
                let default_format = policy.format(&default.id)?;
                let bits = unsafe {
                    std::ptr::read_unaligned(default_format.as_ptr().cast::<WAVEFORMATEX>())
                        .wBitsPerSample
                };
                let desired = virtual_format(channels, bits, false);
                journal.format = Some(FormatChange {
                    id: selected.id.clone(),
                    before,
                    applied: desired.clone(),
                });
                save(directory, &journal)?;
                crate::display_recovery::audio(true)?;
                if policy.set_format(&selected.id, &desired).is_err() {
                    let desired = virtual_format(channels, bits, true);
                    journal.format.as_mut().unwrap().applied = desired.clone();
                    save(directory, &journal)?;
                    policy.set_format(&selected.id, &desired)?;
                }
                journal.format.as_mut().unwrap().applied = policy.format(&selected.id)?;
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
        *active = Some(Active {
            directory: directory.into(),
            sink: selected.id.clone(),
            virtual_sink: managed_virtual,
            users: 1,
        });
        Ok(Self {
            sink: selected.id.clone(),
            directory: directory.into(),
            keep_default: config.boolean("keep_sink_default", true),
            capture_only,
            virtual_sink: managed_virtual,
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
        let _active = ACTIVE.lock().unwrap();
        if !self.virtual_sink || self.capture_only {
            return Ok(());
        }
        let policy = Policy::new()?;
        let current = policy.format(&self.sink)?;
        let format = unsafe { std::ptr::read_unaligned(current.as_ptr().cast::<WAVEFORMATEX>()) };
        if usize::from(format.nChannels) == channels {
            return Ok(());
        }
        let mut journal: Journal = serde_json::from_slice(&std::fs::read(path(&self.directory))?)?;
        let desired = virtual_format(channels, format.wBitsPerSample, false);
        let Some(change) = journal.format.as_mut() else {
            bail!("virtual audio format is not owned");
        };
        change.applied = desired.clone();
        save(&self.directory, &journal)?;
        policy.set_format(&self.sink, &desired)?;
        journal.format.as_mut().unwrap().applied = policy.format(&self.sink)?;
        save(&self.directory, &journal)
    }
}
impl Drop for Route {
    fn drop(&mut self) {
        let mut active = ACTIVE.lock().unwrap();
        let Some(shared) = active.as_mut() else {
            return;
        };
        shared.users -= 1;
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
    fn policy_config_abi_and_surround_formats_match_windows_contract() {
        assert_eq!(
            std::mem::offset_of!(PolicyVtbl, default),
            13 * size_of::<usize>()
        );
        for (channels, mask) in [(2, 3), (6, 0x3f), (8, 0x63f)] {
            let bytes = virtual_format(channels, 24, false);
            let format =
                unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<WAVEFORMATEXTENSIBLE>()) };
            let actual_mask = format.dwChannelMask;
            let rate = format.Format.nAvgBytesPerSec;
            assert_eq!(actual_mask, mask);
            assert_eq!(rate, 48000 * channels as u32 * 3);
        }
    }
}
