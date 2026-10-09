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
    #[serde(default)]
    mode_rate: Option<(Mode, Mode)>,
    hdr: Option<(bool, bool)>,
    #[serde(default)]
    position: Option<(
        butterpollo_core::topology::Position,
        butterpollo_core::topology::Position,
    )>,
    #[serde(default)]
    profile: Option<(Option<String>, String, bool)>,
    #[serde(default)]
    external: bool,
    #[serde(default)]
    audio: bool,
    #[serde(default)]
    arrangement: Option<(
        crate::display::Snapshot,
        Vec<butterpollo_core::topology::Node>,
    )>,
    #[serde(default)]
    baseline: Option<(crate::display::Snapshot, Vec<String>)>,
    #[serde(default)]
    activation: Option<(
        crate::display::Snapshot,
        Vec<butterpollo_core::topology::Node>,
    )>,
}
impl Entry {
    fn pending(&self) -> bool {
        self.mode.is_some()
            || self.hdr.is_some()
            || self.mode_rate.is_some()
            || self.position.is_some()
            || self.profile.is_some()
            || self.external
            || self.audio
            || self.arrangement.is_some()
            || self.baseline.is_some()
            || self.activation.is_some()
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
        // SAFETY: lock() acquired this mutex on the current thread, and its local
        // guard owns the handle until this single release and close.
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
    // SAFETY: name is terminated and lives through CreateMutexW. The returned handle
    // is either closed on failure or transferred to a guard after this thread acquires it.
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
    // SAFETY: OpenProcess supplies an owned query handle; the four initialized outputs
    // remain writable for GetProcessTimes, and the handle is closed exactly once afterward.
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
        if exited.dwLowDateTime != 0 || exited.dwHighDateTime != 0 {
            return Ok(None);
        }
        Ok(Some(
            u64::from(created.dwLowDateTime) | (u64::from(created.dwHighDateTime) << 32),
        ))
    }
}
/// Take the journal and undo what an interrupted host left behind. An
/// incomplete recovery is returned for the log rather than as an error: a
/// display that cannot take back a setting must not stop the host from
/// starting, as it did on every start while the journal stayed pending.
pub fn initialize(directory: &Path) -> Result<Option<anyhow::Error>> {
    let path = directory.join("display-recovery.json");
    let _guard = lock(&path)?;
    let mut incomplete = None;
    if path.exists() {
        let journal: Journal = serde_json::from_slice(&std::fs::read(&path)?)?;
        if identity(journal.pid)? == Some(journal.started) && journal.pid != std::process::id() {
            bail!("another host owns this display recovery journal");
        }
        incomplete = recover(&path).err();
    }
    PATH.set(path)
        .map_err(|_| anyhow::anyhow!("display recovery already initialized"))?;
    Ok(incomplete)
}
fn watch(path: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let mut child = WATCH.lock().unwrap();
    if let Some(process) = child.as_mut()
        && process.try_wait()?.is_none()
    {
        return Ok(());
    }
    let spawn = |flags: u32| -> Result<std::process::Child> {
        Ok(Command::new(std::env::current_exe()?)
            .arg("--display-watch")
            .arg(std::process::id().to_string())
            .arg("--config-dir")
            .arg(path.parent().context("journal directory missing")?)
            .creation_flags(flags)
            .spawn()?)
    };
    // Escape the host's job, otherwise a service stop would also kill recovery.
    // A job that forbids breaking away (a launcher, a terminal) refuses that
    // with access denied, and every frame limit and display change failed on
    // it; the helper then stays in the job and still covers a host crash.
    let started = spawn(CREATE_BREAKAWAY_FROM_JOB.0 | CREATE_NO_WINDOW.0).or_else(|error| {
        tracing::debug!(%error, "display recovery helper cannot leave the host's job");
        spawn(CREATE_NO_WINDOW.0)
    });
    *child = Some(started.context("cannot start the display recovery helper")?);
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
    journal.entries.retain(|_, entry| entry.pending());
    butterpollo_core::state::write_json(path, &journal)?;
    if journal.entries.is_empty() {
        return Ok(());
    }
    if let Err(e) = watch(path) {
        butterpollo_core::state::write_json(path, &prior)?;
        return Err(e);
    }
    Ok(())
}
// A pending entry's original value wins over a newer "before": an entry kept
// for a display that was away holds the value to restore, and a later stream
// would otherwise record the changed value as the original.
pub fn mode(id: &str, output: &str, before: Mode, applied: Mode) -> Result<()> {
    change(id, output, |e| {
        e.mode = Some((e.mode.map_or(before, |(original, _)| original), applied))
    })
}
pub fn mode_rate(id: &str, output: &str, before: Mode, applied: Mode) -> Result<()> {
    change(id, output, |e| {
        e.mode_rate = Some((
            e.mode_rate.map_or(before, |(original, _)| original),
            applied,
        ))
    })
}
pub fn hdr(id: &str, output: &str, before: bool, applied: bool) -> Result<()> {
    change(id, output, |e| {
        e.hdr = Some((e.hdr.map_or(before, |(original, _)| original), applied))
    })
}
pub fn external(pending: bool) -> Result<()> {
    change("external-limiter", "", |e| e.external = pending)
}
pub fn audio(pending: bool) -> Result<()> {
    change("audio-routing", "", |e| e.audio = pending)
}
pub fn arrangement(
    value: Option<(
        crate::display::Snapshot,
        Vec<butterpollo_core::topology::Node>,
    )>,
) -> Result<()> {
    change("display-arrangement", "", |e| e.arrangement = value)
}
pub fn baseline(value: Option<(crate::display::Snapshot, Vec<String>)>) -> Result<()> {
    change("saved-display-baseline", "", |entry| entry.baseline = value)
}
pub fn activation(
    value: Option<(
        crate::display::Snapshot,
        Vec<butterpollo_core::topology::Node>,
    )>,
) -> Result<()> {
    change("display-activation", "", |entry| entry.activation = value)
}
pub fn reset() -> Result<()> {
    let path = PATH.get().context("display recovery is not initialized")?;
    let _guard = lock(path)?;
    recover(path)
}
/// What a kept entry holds for a display, which was away when a stream
/// ended and still has what that stream applied: the original values, and
/// for the mode also the one applied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pending {
    pub hdr: Option<bool>,
    /// (original, applied) width, height and refresh in millihertz.
    pub mode: Option<(Mode, Mode)>,
    pub profile: Option<Option<String>>,
}
/// What an earlier stream left to restore on a display. A stream that takes
/// the display over restores these rather than the values it finds, or its
/// release would clear the entry and leave them changed for good. A journal
/// that cannot be read counts as nothing pending; releasing it fails alike.
pub fn pending(id: &str) -> Pending {
    let Some(path) = PATH.get().filter(|p| p.exists()) else {
        return Pending::default();
    };
    lock(path)
        .and_then(|_guard| pending_in(path, id))
        .unwrap_or_else(|error| {
            tracing::warn!(error = %format!("{error:#}"), "display recovery journal is unreadable");
            Pending::default()
        })
}
pub(crate) fn pending_in(path: &Path, id: &str) -> Result<Pending> {
    let journal: Journal = serde_json::from_slice(&std::fs::read(path)?)?;
    Ok(journal
        .entries
        .get(id)
        .map(|entry| Pending {
            hdr: entry.hdr.map(|(original, _)| original),
            mode: entry.mode_rate,
            profile: entry
                .profile
                .as_ref()
                .map(|(original, ..)| original.clone()),
        })
        .unwrap_or_default())
}
pub fn release(id: &str) -> Result<()> {
    let Some(path) = PATH.get().filter(|p| p.exists()) else {
        return Ok(());
    };
    let _guard = lock(path)?;
    release_in(path, id)
}
pub(crate) fn release_in(path: &Path, id: &str) -> Result<()> {
    let mut journal: Journal = serde_json::from_slice(&std::fs::read(path)?)?;
    if let Some(entry) = journal.entries.get_mut(id) {
        entry.mode = None;
        entry.mode_rate = None;
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
        let original = e.profile.take().map_or(before, |(original, ..)| original);
        e.profile = Some((original, applied.into(), system))
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
/// Undo what an interrupted host changed. Every step is attempted even when
/// an earlier one fails, and the failures are reported together. A display
/// that is not connected now, such as a TV that was switched off, keeps its
/// own HDR, colour profile, mode and position entries for a later recovery:
/// Windows remembers those settings per display, so they would otherwise
/// stay changed. Layout entries are attempted once.
fn recover(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let journal: Journal = serde_json::from_slice(&std::fs::read(path)?)?;
    let directory = path.parent().context("journal directory missing")?;
    let mut failures = Vec::new();
    let mut step = |what: &str, result: Result<()>| {
        if let Err(error) = result {
            failures.push(format!("{what}: {error:#}"));
        }
    };
    if journal.entries.values().any(|e| e.external) {
        step("frame limiter", crate::limiter::recover(directory));
    }
    if journal.entries.values().any(|e| e.audio) {
        step("audio routing", crate::audio_route::recover(directory));
    }
    for entry in journal.entries.values() {
        if let Some((before, applied)) = &entry.arrangement {
            step(
                "display arrangement",
                crate::display_arrangement::unchanged(applied).and_then(|unchanged| {
                    if unchanged { before.restore() } else { Ok(()) }
                }),
            );
        }
    }
    let monitors = crate::display::monitors().unwrap_or_else(|error| {
        step("display list", Err(error));
        Vec::new()
    });
    let mut kept = Journal::default();
    for (id, entry) in &journal.entries {
        let Some(m) = monitors.iter().find(|m| &m.device_id == id) else {
            let display = Entry {
                output: entry.output.clone(),
                profile: entry.profile.clone(),
                hdr: entry.hdr,
                mode: entry.mode,
                mode_rate: entry.mode_rate,
                position: entry.position,
                ..Default::default()
            };
            // Each setting is restored later only if the display still has
            // the value the stream applied.
            if display.pending() {
                kept.entries.insert(id.clone(), display);
            }
            continue;
        };
        let what = |setting: &str| format!("{} {setting}", m.display_name);
        if let Some((previous, applied, system)) = &entry.profile {
            step(
                &what("colour profile"),
                crate::hdr_profile::restore(m, previous.as_deref(), applied, *system),
            );
        }
        if let Some((previous, applied)) = entry.hdr
            && m.hdr_enabled == applied
        {
            step(&what("HDR"), crate::display::set_hdr(m, previous));
        }
        if let Some((previous, applied)) = entry.mode {
            step(
                &what("mode"),
                crate::display::mode(&m.display_name).and_then(|current| {
                    if (
                        current.dmPelsWidth,
                        current.dmPelsHeight,
                        current.dmDisplayFrequency,
                    ) == applied
                    {
                        crate::display::set_mode(
                            &m.display_name,
                            previous.0,
                            previous.1,
                            previous.2,
                        )
                    } else {
                        Ok(())
                    }
                }),
            );
        }
        if let Some((previous, applied)) = entry.mode_rate {
            step(
                &what("mode and refresh"),
                (|| {
                    let current = crate::display::mode(&m.display_name)?;
                    let rate = crate::display::Topology::query()?.refresh(id)?;
                    if (current.dmPelsWidth, current.dmPelsHeight, rate.0) == applied {
                        crate::display::Topology::set_mode_rate(
                            id,
                            previous.0,
                            previous.1,
                            butterpollo_core::framegen::Rate(previous.2),
                        )?;
                    }
                    Ok(())
                })(),
            );
        }
    }
    step(
        "display positions",
        (|| {
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
            Ok(())
        })(),
    );
    for entry in journal.entries.values() {
        if let Some((before, applied)) = &entry.activation {
            step(
                "display activation",
                (|| {
                    if applied.is_empty() || crate::display_arrangement::unchanged(applied)? {
                        before.restore()?;
                    }
                    Ok(())
                })(),
            );
        }
        if let Some((snapshot, excluded)) = &entry.baseline {
            step("saved display layout", snapshot.restore_excluding(excluded));
        }
    }
    // Keep a valid document so interrupted reads never see partial JSON. The
    // kept entries have no owner until the next host records a change.
    butterpollo_core::state::write_json(path, &kept)?;
    if !failures.is_empty() {
        bail!("display recovery incomplete: {}", failures.join("; "));
    }
    Ok(())
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
    // SAFETY: The successfully opened process handle has synchronization rights and
    // remains owned until the wait finishes; every exit path closes it exactly once.
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_kept_entry_gives_its_originals_until_released() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("display-recovery.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "pid": 0,
                "started": 0,
                "entries": {"tv": {
                    "output": r"\\.\DISPLAY2",
                    "mode": null,
                    "hdr": [false, true],
                    "mode_rate": [[3840, 2160, 60000], [1920, 1080, 120000]],
                    "profile": [null, "hdr.icm", false],
                }},
            }))?,
        )?;
        assert_eq!(
            pending_in(&path, "tv")?,
            Pending {
                hdr: Some(false),
                mode: Some(((3840, 2160, 60000), (1920, 1080, 120000))),
                profile: Some(None),
            }
        );
        assert_eq!(pending_in(&path, "monitor")?, Pending::default());
        // Releasing the display settings keeps the colour profile's entry.
        release_in(&path, "tv")?;
        assert_eq!(
            pending_in(&path, "tv")?,
            Pending {
                profile: Some(None),
                ..Default::default()
            }
        );
        Ok(())
    }
    #[test]
    fn exited_process_handles_do_not_keep_recovery_journals_owned() {
        use std::os::windows::process::CommandExt;
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/C", "exit", "0"])
            .current_dir(std::env::temp_dir())
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        // Child still retains its process handle here, just like SCM/watchers.
        assert_eq!(identity(pid).unwrap(), None);
        assert!(identity(std::process::id()).unwrap().is_some());
    }
}
