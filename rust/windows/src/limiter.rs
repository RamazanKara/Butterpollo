//! Stream-owned limiter transactions. Durable originals precede every mutation.
use crate::{
    nvapi::{Drs, Scope, Value},
    rtss,
};
use anyhow::{Result, bail};
use butterpollo_core::{
    config::Config,
    framegen::{Policy, Provider},
    state::write_json,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
const FRAME_LIMIT: u32 = 0x10835002;
const VSYNC: u32 = 0x00a879cf;
#[derive(Clone, Serialize, Deserialize)]
struct NvChange {
    #[serde(default)]
    scope: Scope,
    id: u32,
    before: Value,
    applied: u32,
}
#[derive(Clone, Serialize, Deserialize)]
struct RtssChange {
    root: PathBuf,
    before: BTreeMap<String, Option<u32>>,
    applied: BTreeMap<String, Option<u32>>,
    disabled: bool,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Journal {
    version: u32,
    rtss: Option<RtssChange>,
    nvidia: Vec<NvChange>,
}
#[derive(Default)]
struct State {
    users: usize,
    active: String,
    directory: PathBuf,
    process: Option<(PathBuf, crate::process::Process)>,
    message: String,
}
fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Mutex::default)
}
fn path(directory: &Path) -> PathBuf {
    directory.join("frame-limiter-recovery.json")
}
fn load(directory: &Path) -> Result<Journal> {
    match std::fs::metadata(path(directory)) {
        Ok(m) if m.len() <= 1024 * 1024 => {
            let j: Journal = serde_json::from_slice(&std::fs::read(path(directory))?)?;
            if j.version != 1 {
                bail!("unsupported frame limiter recovery version");
            }
            Ok(j)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Journal {
            version: 1,
            ..Default::default()
        }),
        _ => bail!("frame limiter recovery is unreadable or exceeds its size limit"),
    }
}
fn persist(directory: &Path, journal: &Journal) -> Result<()> {
    write_json(&path(directory), journal)?;
    crate::display_recovery::external(true)
}
/// Also used by the Rust crash watcher. Failures leave the journal intact.
pub fn recover(directory: &Path) -> Result<()> {
    let mut journal = load(directory)?;
    if let Some(change) = journal.rtss.as_ref() {
        let text = rtss::read(&change.root)?;
        let current = rtss::properties(&text)?;
        let mut restore = BTreeMap::new();
        // A user edit made after our application takes precedence on restore.
        for key in rtss::KEYS {
            if current.get(key) == change.applied.get(key) {
                restore.insert(key.into(), change.before.get(key).copied().flatten());
            }
        }
        // Restore the durable profile even if RTSS's message loop is stalled.
        // Keep the journal until the helper also confirms the live state.
        if !restore.is_empty() {
            rtss::write_profile(&change.root, &restore)?;
        }
        if rtss::available(&change.root) {
            let _process = rtss::start(&change.root)?;
            let before = rtss::wait_ready(&change.root)?;
            let disabled = if before.flags & 4 == 0 {
                Some(change.disabled)
            } else {
                None
            };
            let restored = rtss::reload(&change.root, disabled)?;
            for (key, value) in &restore {
                if let Some(value) = value
                    && restored.values.get(key) != Some(&Some(*value))
                {
                    bail!("RTSS did not confirm restoration of {key}");
                }
            }
            if disabled.is_some() && (restored.flags & 4 != 0) != change.disabled {
                bail!("RTSS did not restore its original enable state");
            }
        } else if rtss::running(&change.root) {
            bail!(
                "RTSS is still running but its hooks are unavailable; retaining exclusive limiter ownership"
            );
        }
        journal.rtss = None;
        write_json(&path(directory), &journal)?;
    }
    if !journal.nvidia.is_empty() {
        for change in &journal.nvidia {
            let nv = match Drs::open_scope(&change.scope, false) {
                Ok(nv) => nv,
                Err(error)
                    if error
                        .downcast_ref::<crate::nvapi::NvError>()
                        .is_some_and(|e| e.code == -166) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            let current = nv.read(change.id)?;
            if current.override_value == Some(change.applied) {
                nv.write(change.id, change.before.override_value)?;
                nv.save()?;
            }
        }
        for change in &journal.nvidia {
            let nv = match Drs::open_scope(&change.scope, false) {
                Ok(nv) => nv,
                Err(error)
                    if error
                        .downcast_ref::<crate::nvapi::NvError>()
                        .is_some_and(|e| e.code == -166) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            let current = nv.read(change.id)?;
            if current.effective == change.applied
                && current.override_value != change.before.override_value
            {
                bail!(
                    "NVIDIA did not confirm restoration of setting {:08x}",
                    change.id
                );
            }
        }
        journal.nvidia.clear();
        write_json(&path(directory), &journal)?;
    }
    Ok(())
}
fn apply_nvidia(
    directory: &Path,
    journal: &mut Journal,
    policy: &Policy,
    limit: bool,
    disable: bool,
) -> Result<()> {
    let nv = Drs::open()?;
    let mut desired = Vec::new();
    if limit || disable {
        desired.push((
            FRAME_LIMIT,
            if limit {
                policy.rate.rounded().clamp(1, 1023)
            } else {
                0
            },
        ));
    }
    if policy.disable_vsync {
        desired.push((VSYNC, 0x08416747));
    }
    if policy.smooth_motion {
        if nv.version()? < 57186 {
            bail!("NVIDIA Smooth Motion requires driver 571.86 or newer");
        }
        desired.extend([
            (0xb0d384c0, 1),
            (0xb0cc0875, 7),
            (0x0005f543, 2),
            (0x10835000, 1),
        ]);
    }
    if desired.is_empty() {
        return Ok(());
    }
    for (id, applied) in desired {
        let before = nv.read(id)?;
        if before.effective != applied {
            journal.nvidia.push(NvChange {
                scope: Scope::Global,
                id,
                before,
                applied,
            });
        }
    }
    persist(directory, journal)?;
    for change in journal.nvidia.iter().filter(|c| c.scope == Scope::Global) {
        nv.write(change.id, Some(change.applied))?;
    }
    nv.save()?;
    drop(nv);
    let nv = Drs::open()?;
    for change in journal.nvidia.iter().filter(|c| c.scope == Scope::Global) {
        if nv.read(change.id)?.effective != change.applied {
            bail!("NVIDIA did not confirm the requested override");
        }
    }
    Ok(())
}
fn apply_preferences(directory: &Path, journal: &mut Journal, config: &Config) -> Result<()> {
    let mut desired = Vec::new();
    if config.boolean("nvenc_opengl_vulkan_on_dxgi", true) {
        desired.push((Scope::Base, 0x20d690f8, 1));
    }
    if config.boolean("nvenc_latency_over_power", true) {
        desired.push((
            Scope::Application(std::env::current_exe()?.to_string_lossy().into_owned()),
            0x1057eb71,
            1,
        ));
    }
    for (scope, id, applied) in desired {
        let nv = Drs::open_scope(&scope, true)?;
        let before = nv.read(id)?;
        let needs_override = match scope {
            Scope::Application(_) => before.override_value != Some(applied),
            _ => before.effective != applied,
        };
        if !needs_override {
            continue;
        }
        journal.nvidia.push(NvChange {
            scope: scope.clone(),
            id,
            before,
            applied,
        });
        persist(directory, journal)?;
        nv.write(id, Some(applied))?;
        nv.save()?;
        drop(nv);
        if Drs::open_scope(&scope, false)?.read(id)?.override_value != Some(applied) {
            bail!("NVIDIA did not confirm driver preference {id:08x}");
        }
    }
    Ok(())
}
pub struct Lease;
impl Lease {
    pub fn acquire(directory: &Path, config: &Config, policy: &Policy) -> Result<Self> {
        let mut state = state().lock().unwrap();
        if state.users != 0 {
            state.users += 1;
            return Ok(Self);
        }
        state.directory = directory.into();
        state.active = "none".into();
        state.message.clear();
        if let Err(error) = recover(directory) {
            state.active = "rtss".into();
            state.message = format!("{error:#}");
            tracing::warn!(error = %state.message, "frame limiter recovery remains pending; retaining exclusive limiter ownership");
            state.users = 1;
            return Ok(Self);
        }
        let mut journal = load(directory)?;
        let root = rtss::root(config);
        let use_rtss = policy.enabled
            && matches!(policy.provider, Provider::Auto | Provider::Rtss)
            && rtss::available(&root);
        if !use_rtss
            || state
                .process
                .as_ref()
                .is_some_and(|(owned_root, _)| owned_root != &root)
        {
            state.process.take();
        }
        if use_rtss {
            let applied = (|| -> Result<()> {
                // A failed restoration retains our process until recovery
                // succeeds. start() returns None for that still-running RTSS;
                // replacing its owner with None would kill it on reconnect.
                if let Some(process) = rtss::start(&root)? {
                    state.process = Some((root.clone(), process));
                }
                let reply = rtss::wait_ready(&root)?;
                let text = rtss::read(&root)?;
                let (numerator, denominator) = policy.rate.rational();
                let change = RtssChange {
                    root: root.clone(),
                    before: rtss::properties(&text)?,
                    applied: BTreeMap::from([
                        ("Limit".into(), Some(numerator)),
                        ("LimitDenominator".into(), Some(denominator)),
                        ("SyncLimiter".into(), Some(policy.sync_limiter)),
                    ]),
                    disabled: reply.flags & 4 != 0,
                };
                journal.rtss = Some(change.clone());
                persist(directory, &journal)?;
                let reply = rtss::apply(&root, &change.applied, Some(false))?;
                if reply.flags & 4 != 0 || reply.values != change.applied {
                    bail!("RTSS did not confirm the requested rational frame limit");
                }
                Ok(())
            })();
            match applied {
                Ok(()) => {
                    state.active = "rtss".into();
                    tracing::info!(rate = ?policy.rate.rational(), sync_limiter = policy.sync_limiter, "RTSS frame limit applied and verified");
                }
                Err(error) => {
                    state.message = format!("{error:#}");
                    tracing::warn!(root = %root.display(), error = %state.message, "RTSS frame limiter could not be applied");
                    if journal.rtss.is_some() && recover(directory).is_err() {
                        state.active = "rtss".into();
                    } else {
                        journal = load(directory)?;
                    }
                }
            }
        }
        let limit = policy.enabled
            && state.active == "none"
            && matches!(
                policy.provider,
                Provider::Auto | Provider::Rtss | Provider::Nvidia
            );
        let disable = state.active == "rtss";
        // An unresolved RTSS mutation forbids enabling a second cap. Driver
        // VSYNC/Smooth Motion overrides remain independent of the cap itself.
        if (limit || disable || policy.disable_vsync || policy.smooth_motion) && Drs::open().is_ok()
        {
            if let Err(error) = apply_nvidia(directory, &mut journal, policy, limit, disable) {
                state.message = error.to_string();
                tracing::warn!(%error,"NVIDIA frame limiter overrides failed");
            } else if limit {
                state.active = "nvidia-control-panel".into();
            }
        }
        if state.active == "none" && policy.enabled {
            tracing::warn!(provider = policy.provider.name(), rtss_root = %root.display(), rtss_available = rtss::available(&root), error = %state.message, "frame limiter has no available provider");
        }
        if Drs::open().is_ok()
            && let Err(error) = apply_preferences(directory, &mut journal, config)
        {
            tracing::warn!(%error, "NVIDIA driver preferences could not be applied");
            state.message = error.to_string();
        }
        state.users = 1;
        Ok(Self)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = state().lock().unwrap();
        state.users -= 1;
        if state.users != 0 {
            return;
        }
        match recover(&state.directory) {
            Ok(()) => {
                let _ = crate::display_recovery::external(false);
                state.active = "none".into();
                state.message.clear();
                state.process.take();
            }
            Err(error) => {
                state.message = error.to_string();
                tracing::warn!(%error,"frame limiter restoration remains pending");
            }
        }
    }
}
pub fn status(config: &Config) -> Json {
    let root = rtss::root(config);
    let available = rtss::available(&root);
    let nvcp = Drs::open().is_ok();
    let state = state().lock().unwrap();
    let configured = config.get("rtss_install_path", config.get("rtss_path", ""));
    json!({"status":true,"enabled":config.boolean("frame_limiter_enable",false),
        "configured_provider":Provider::parse(config.get("frame_limiter_provider","auto")).name(),
        "active_provider":if state.active.is_empty(){"none"}else{&state.active},"nvidia_available":nvcp,"nvcp_ready":nvcp,
        "rtss_available":available,"disable_vsync":config.boolean("frame_limiter_disable_vsync",config.boolean("rtss_disable_vsync_ullm",false)),
        "disable_vsync_ullm":config.boolean("frame_limiter_disable_vsync",config.boolean("rtss_disable_vsync_ullm",false)),
        "nv_overrides_supported":nvcp,"configured_path":configured,"path_configured":!configured.is_empty(),
        "resolved_path":root,"path_exists":root.is_dir(),"hooks_found":available,"profile_found":root.join("Profiles/Global").is_file(),
        "can_bootstrap_profile":available,"process_running":rtss::running(&root),
        "message":if state.message.is_empty(){"Limiter settings apply during streaming and restore after the last stream."}else{&state.message}})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_nvidia_recovery_journals_keep_their_global_scope() {
        let change: NvChange = serde_json::from_str(
            r#"{"id":123,"before":{"effective":0,"override_value":null},"applied":1}"#,
        )
        .unwrap();
        assert_eq!(change.scope, Scope::Global);
        let mut app = change;
        app.scope = Scope::Application("C:\\Butterpollo\\butterpollo.exe".into());
        let restored: NvChange =
            serde_json::from_value(serde_json::to_value(&app).unwrap()).unwrap();
        assert_eq!(restored.scope, app.scope);
    }
}
