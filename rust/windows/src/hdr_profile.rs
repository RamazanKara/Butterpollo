//! Windows Advanced Color ICC associations, owned by the last streaming lease.
//! ABI: https://learn.microsoft.com/windows/win32/api/icm/nf-icm-colorprofilegetdisplaydefault
use crate::display::Monitor;
use anyhow::{Context, Result, bail};
use libloading::Library;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};
use windows::{
    Win32::Foundation::{HLOCAL, LUID, LocalFree},
    core::HRESULT,
};

type Get = unsafe extern "system" fn(u32, LUID, u32, u32, u32, *mut *mut u16) -> i32;
type Add = unsafe extern "system" fn(u32, *const u16, LUID, u32, i32, i32) -> i32;
type Remove = unsafe extern "system" fn(u32, *const u16, LUID, u32, i32) -> i32;
struct Api {
    _library: Library,
    get: Get,
    add: Add,
    remove: Remove,
}
impl Api {
    fn load() -> Result<Self> {
        let directory = std::env::var_os("SystemRoot").context("Windows directory unavailable")?;
        unsafe {
            let library = Library::new(Path::new(&directory).join("System32").join("mscms.dll"))?;
            Ok(Self {
                get: *library.get(b"ColorProfileGetDisplayDefault\0")?,
                add: *library.get(b"ColorProfileAddDisplayAssociation\0")?,
                remove: *library.get(b"ColorProfileRemoveDisplayAssociation\0")?,
                _library: library,
            })
        }
    }
    fn get(&self, monitor: &Monitor, system: bool) -> Result<Option<String>> {
        unsafe {
            let mut pointer = std::ptr::null_mut();
            let status = HRESULT((self.get)(
                scope(system),
                monitor.adapter,
                monitor.source,
                0,
                8,
                &mut pointer,
            ));
            // Windows returns no association either as a null name or NOT_FOUND.
            if pointer.is_null() {
                if status.0 == 0x80070490u32 as i32 || status.0 == 0x80070002u32 as i32 {
                    return Ok(None);
                }
                status.ok()?;
                return Ok(None);
            }
            let result = (|| {
                status.ok()?;
                let length = (0..32768)
                    .find(|&n| *pointer.add(n) == 0)
                    .context("color profile name is too long")?;
                let name = String::from_utf16(std::slice::from_raw_parts(pointer, length))?;
                Ok((!name.is_empty()).then_some(name))
            })();
            let _ = LocalFree(Some(HLOCAL(pointer.cast())));
            result
        }
    }
    fn add(&self, monitor: &Monitor, system: bool, name: &str) -> Result<()> {
        let name = wide(name)?;
        unsafe {
            HRESULT((self.add)(
                scope(system),
                name.as_ptr(),
                monitor.adapter,
                monitor.source,
                1,
                1,
            ))
            .ok()?;
        }
        Ok(())
    }
    fn remove(&self, monitor: &Monitor, system: bool, name: &str) -> Result<()> {
        let name = wide(name)?;
        unsafe {
            HRESULT((self.remove)(
                scope(system),
                name.as_ptr(),
                monitor.adapter,
                monitor.source,
                1,
            ))
            .ok()?;
        }
        Ok(())
    }
}
fn scope(system: bool) -> u32 {
    if system { 0 } else { 1 }
}
fn wide(name: &str) -> Result<Vec<u16>> {
    if name.is_empty() || name.len() > 32767 || name.contains('\0') {
        bail!("invalid color profile name");
    }
    Ok(name.encode_utf16().chain(Some(0)).collect())
}
fn filename(selection: &str) -> Result<&str> {
    if selection.is_empty()
        || selection.contains(['/', '\\', ':', '\0'])
        || matches!(selection, "." | "..")
    {
        bail!("color profile must be an installed filename");
    }
    Ok(selection)
}
fn installed(selection: &str) -> Result<String> {
    let selection = filename(selection)?;
    let root = std::env::var_os("SystemRoot").context("Windows directory unavailable")?;
    let directory = PathBuf::from(root)
        .join("System32")
        .join("spool")
        .join("drivers")
        .join("color");
    let names = if Path::new(selection).extension().is_none() {
        vec![
            selection.into(),
            format!("{selection}.icm"),
            format!("{selection}.icc"),
        ]
    } else {
        vec![selection.into()]
    };
    names
        .into_iter()
        .find(|name| directory.join(name).is_file())
        .context("selected ICC profile is not installed")
}
struct State {
    users: usize,
    previous: Option<String>,
    applied: String,
    system: bool,
}
static LEASES: Mutex<BTreeMap<String, State>> = Mutex::new(BTreeMap::new());
pub struct Lease {
    identity: String,
}
impl Lease {
    pub fn acquire(output: &str, selection: &str) -> Result<Self> {
        let applied = installed(selection)?;
        let monitor = crate::display::monitors()?
            .into_iter()
            .find(|m| m.display_name == output || m.device_id == output)
            .context("ICC output disappeared")?;
        let mut leases = LEASES.lock().unwrap();
        if let Some(state) = leases.get_mut(&monitor.device_id) {
            if !state.applied.eq_ignore_ascii_case(&applied) {
                bail!("another client owns a different ICC profile on this output");
            }
            state.users += 1;
        } else {
            let api = Api::load()?;
            let system = crate::process::is_system();
            let previous = api.get(&monitor, system)?;
            if !previous
                .as_ref()
                .is_some_and(|name| name.eq_ignore_ascii_case(&applied))
            {
                crate::display_recovery::profile(
                    &monitor.device_id,
                    &monitor.display_name,
                    previous.clone(),
                    &applied,
                    system,
                )?;
                // Preserve the recovery entry if a failed call partially changes Windows.
                if let Err(error) = api.add(&monitor, system, &applied) {
                    if restore(&monitor, previous.as_deref(), &applied, system).is_ok() {
                        let _ = crate::display_recovery::release_profile(&monitor.device_id);
                    }
                    return Err(error);
                }
            }
            leases.insert(
                monitor.device_id.clone(),
                State {
                    users: 1,
                    previous,
                    applied,
                    system,
                },
            );
        }
        Ok(Self {
            identity: monitor.device_id,
        })
    }
}
/// Used by the separate recovery process as well as normal lease teardown.
pub fn restore(
    monitor: &Monitor,
    previous: Option<&str>,
    applied: &str,
    system: bool,
) -> Result<()> {
    if previous.is_some_and(|name| name.eq_ignore_ascii_case(applied)) {
        return Ok(());
    }
    let api = Api::load()?;
    if api
        .get(monitor, system)?
        .as_deref()
        .is_some_and(|current| current.eq_ignore_ascii_case(applied))
    {
        if let Some(previous) = previous {
            api.add(monitor, system, previous)?;
        } else {
            api.remove(monitor, system, applied)?;
        }
    }
    Ok(())
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut leases = LEASES.lock().unwrap();
        let Some(state) = leases.get_mut(&self.identity) else {
            return;
        };
        state.users -= 1;
        if state.users != 0 {
            return;
        }
        let state = leases.remove(&self.identity).unwrap();
        let result = (|| {
            if let Some(monitor) = crate::display::monitors()?
                .iter()
                .find(|m| m.device_id == self.identity)
            {
                restore(
                    monitor,
                    state.previous.as_deref(),
                    &state.applied,
                    state.system,
                )?;
            }
            crate::display_recovery::release_profile(&self.identity)
        })();
        if let Err(error) = result {
            tracing::warn!(%error, "HDR profile restoration deferred to recovery");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installed_profile_selection_cannot_escape_the_color_directory() {
        for bad in [
            "",
            ".",
            "..",
            "..\\other.icc",
            "C:\\other.icc",
            "other:stream",
            "a/b.icc",
            "a\0b.icc",
        ] {
            assert!(filename(bad).is_err());
        }
        assert_eq!(
            filename("My HDR Profile.icc").unwrap(),
            "My HDR Profile.icc"
        );
    }
}
