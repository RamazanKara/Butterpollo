//! Durable display restoration after an abort, crash or service termination.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Mutex, OnceLock},
};
use windows::Win32::{Foundation::*, System::Threading::*};
type Mode = (u32, u32, u32);
#[derive(Clone, Default, Serialize, Deserialize)]
struct Entry {
    output: String,
    mode: Option<(Mode, Mode)>,
    hdr: Option<(bool, bool)>,
    #[serde(default)]
    position: Option<(
        butterpollo_core::topology::Position,
        butterpollo_core::topology::Position,
    )>,
    #[serde(default)]
    profile: Option<(Option<String>, String, bool)>,
}
impl Entry {
    fn pending(&self) -> bool {
        self.mode.is_some()
            || self.hdr.is_some()
            || self.position.is_some()
            || self.profile.is_some()
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Journal {
    pid: u32,
    started: u64,
    entries: BTreeMap<String, Entry>,
}
static PATH: OnceLock<PathBuf> = OnceLock::new();
static WATCH: Mutex<Option<Child>> = Mutex::new(None);
struct JournalLock(HANDLE);
impl Drop for JournalLock {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}
fn lock(path: &Path) -> Result<JournalLock> {
    let hash = butterpollo_core::crypto::hash(path.to_string_lossy().to_lowercase().as_bytes());
    let name: Vec<u16> = format!(
        "Local\\Butterpollo.DisplayJournal.{:x}\0",
        u64::from_le_bytes(hash[..8].try_into().unwrap())
    )
    .encode_utf16()
    .collect();
    unsafe {
        let handle = CreateMutexW(None, false, windows::core::PCWSTR(name.as_ptr()))?;
        let result = WaitForSingleObject(handle, 30000);
        if result != WAIT_OBJECT_0 && result != WAIT_ABANDONED {
            let _ = CloseHandle(handle);
            bail!("display journal is busy");
        }
        Ok(JournalLock(handle))
    }
}

fn identity(pid: u32) -> Result<Option<u64>> {
    unsafe {
        if pid == 0 {
            return Ok(None);
        }
        let process = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => handle,
            Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let result = GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user);
        let _ = CloseHandle(process);
        result?;
        Ok(Some(
            u64::from(created.dwLowDateTime) | (u64::from(created.dwHighDateTime) << 32),
        ))
    }
}
pub fn initialize(directory: &Path) -> Result<()> {
    let path = directory.join("display-recovery.json");
    let _guard = lock(&path)?;
    if path.exists() {
        let journal: Journal = serde_json::from_slice(&std::fs::read(&path)?)?;
        if identity(journal.pid)? == Some(journal.started) && journal.pid != std::process::id() {
            bail!("another host owns this display recovery journal");
        }
        recover(&path)?;
    }
    PATH.set(path)
        .map_err(|_| anyhow::anyhow!("display recovery already initialized"))?;
    Ok(())
}
fn watch(path: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let mut child = WATCH.lock().unwrap();
    if let Some(process) = child.as_mut()
        && process.try_wait()?.is_none()
    {
        return Ok(());
    }
    // Escape the host's job, otherwise a service stop would also kill recovery.
    *child = Some(
        Command::new(std::env::current_exe()?)
            .arg("--display-watch")
            .arg(std::process::id().to_string())
            .arg("--config-dir")
            .arg(path.parent().context("journal directory missing")?)
            .creation_flags(CREATE_BREAKAWAY_FROM_JOB.0 | CREATE_NO_WINDOW.0)
            .spawn()
            .context("cannot start the display recovery helper")?,
    );
    Ok(())
}
fn change(id: &str, output: &str, update: impl FnOnce(&mut Entry)) -> Result<()> {
    let path = PATH.get().context("display recovery is not initialized")?;
    let _guard = lock(path)?;
    let mut journal: Journal = if path.exists() {
        serde_json::from_slice(&std::fs::read(path)?)?
    } else {
        Journal::default()
    };
    let prior = journal.clone();
    journal.pid = std::process::id();
    journal.started = identity(journal.pid)?.context("host identity unavailable")?;
    let entry = journal.entries.entry(id.into()).or_default();
    entry.output = output.into();
    update(entry);
    butterpollo_core::state::write_json(path, &journal)?;
    if let Err(e) = watch(path) {
        butterpollo_core::state::write_json(path, &prior)?;
        return Err(e);
    }
    Ok(())
}
pub fn mode(id: &str, output: &str, before: Mode, applied: Mode) -> Result<()> {
    change(id, output, |e| e.mode = Some((before, applied)))
}
pub fn hdr(id: &str, output: &str, before: bool, applied: bool) -> Result<()> {
    change(id, output, |e| e.hdr = Some((before, applied)))
}
pub fn release(id: &str) -> Result<()> {
    let Some(path) = PATH.get().filter(|p| p.exists()) else {
        return Ok(());
    };
    let _guard = lock(path)?;
    let mut journal: Journal = serde_json::from_slice(&std::fs::read(path)?)?;
    if let Some(entry) = journal.entries.get_mut(id) {
        entry.mode = None;
        entry.hdr = None;
    }
    journal.entries.retain(|_, e| e.pending());
    butterpollo_core::state::write_json(path, &journal)
}
pub fn position(
    id: &str,
    output: &str,
    before: butterpollo_core::topology::Position,
    applied: butterpollo_core::topology::Position,
) -> Result<()> {
    change(id, output, |e| e.position = Some((before, applied)))
}
pub fn release_position(id: &str) -> Result<()> {
    let Some(path) = PATH.get().filter(|p| p.exists()) else {
        return Ok(());
    };
    let _guard = lock(path)?;
    let mut journal: Journal = serde_json::from_slice(&std::fs::read(path)?)?;
    if let Some(entry) = journal.entries.get_mut(id) {
        entry.position = None;
    }
    journal.entries.retain(|_, e| e.pending());
    butterpollo_core::state::write_json(path, &journal)
}
pub fn profile(
    id: &str,
    output: &str,
    before: Option<String>,
    applied: &str,
    system: bool,
) -> Result<()> {
    change(id, output, |e| {
        e.profile = Some((before, applied.into(), system))
    })
}
pub fn release_profile(id: &str) -> Result<()> {
    let Some(path) = PATH.get().filter(|p| p.exists()) else {
        return Ok(());
    };
    let _guard = lock(path)?;
    let mut journal: Journal = serde_json::from_slice(&std::fs::read(path)?)?;
    if let Some(entry) = journal.entries.get_mut(id) {
        entry.profile = None;
    }
    journal.entries.retain(|_, e| e.pending());
    butterpollo_core::state::write_json(path, &journal)
}
fn recover(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let journal: Journal = serde_json::from_slice(&std::fs::read(path)?)?;
    let monitors = crate::display::monitors()?;
    for (id, entry) in &journal.entries {
        let Some(m) = monitors.iter().find(|m| &m.device_id == id) else {
            continue;
        };
        if let Some((previous, applied, system)) = &entry.profile {
            crate::hdr_profile::restore(m, previous.as_deref(), applied, *system)?;
        }
        if let Some((previous, applied)) = entry.hdr
            && m.hdr_enabled == applied
        {
            crate::display::set_hdr(m, previous)?;
        }
        if let Some((previous, applied)) = entry.mode {
            let current = crate::display::mode(&m.display_name)?;
            if (
                current.dmPelsWidth,
                current.dmPelsHeight,
                current.dmDisplayFrequency,
            ) == applied
            {
                crate::display::set_mode(&m.display_name, previous.0, previous.1, previous.2)?;
            }
        }
    }
    let mut topology = crate::display::Topology::query()?;
    let nodes = topology.nodes()?;
    let positions = journal
        .entries
        .iter()
        .filter_map(|(id, entry)| {
            let (before, applied) = entry.position?;
            nodes
                .iter()
                .find(|n| &n.device_id == id && n.desired_position == applied)
                .map(|_| (id.clone(), before))
        })
        .collect::<BTreeMap<_, _>>();
    if !positions.is_empty() {
        topology.set_positions(&positions)?;
    }
    // Keep a valid empty document so interrupted reads never see partial JSON.
    butterpollo_core::state::write_json(path, &Journal::default())
}
pub fn wait_and_recover(pid: u32, directory: &Path) -> Result<()> {
    let path = directory.join("display-recovery.json");
    let owner: Journal = if path.exists() {
        serde_json::from_slice(&std::fs::read(&path)?)?
    } else {
        Journal::default()
    };
    if owner.pid != pid {
        return Ok(());
    }
    unsafe {
        match OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        ) {
            Ok(process) => {
                let identity = identity(pid);
                if identity.is_err() {
                    let _ = CloseHandle(process);
                }
                let result = if identity? == Some(owner.started) {
                    WaitForSingleObject(process, u32::MAX)
                } else {
                    WAIT_OBJECT_0
                };
                let _ = CloseHandle(process);
                if result != WAIT_OBJECT_0 {
                    bail!("display watcher wait failed");
                }
            }
            Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
            }
            Err(e) => return Err(e.into()),
        }
    }
    let _guard = lock(&path)?;
    if path.exists() {
        let journal: Journal = serde_json::from_slice(&std::fs::read(&path)?)?;
        // A newly started host may already have reclaimed the journal.
        if journal.pid != pid {
            return Ok(());
        }
    }
    recover(&path)
}
