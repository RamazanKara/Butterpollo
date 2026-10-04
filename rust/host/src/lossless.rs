//! Lossless Scaling for a stream, as in Vibepollo: once the game runs, a
//! temporary "Vibeshine" game profile limited to the game's programs is
//! added to Lossless Scaling's settings, Lossless Scaling is restarted, and
//! its hotkey starts scaling the game. When the app stops, Lossless Scaling
//! is closed and the profile removed.
use anyhow::{Context, Result, bail};
use butterpollo_core::lossless::{Options, Profile};
use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use xmltree::{Element, EmitterConfig, XMLNode};

/// The profile's title; Vibepollo's, so either host removes the other's.
const TITLE: &str = "Vibeshine";

fn parse(text: &str) -> Result<Element> {
    Ok(Element::parse(
        text.trim_start_matches('\u{feff}').as_bytes(),
    )?)
}
fn write(root: &Element) -> Result<String> {
    let mut bytes = vec![];
    root.write_with_config(
        &mut bytes,
        EmitterConfig::new()
            .perform_indent(true)
            .indent_string("  "),
    )?;
    Ok(String::from_utf8(bytes)?)
}
fn text_of(element: &Element, name: &str) -> String {
    element
        .get_child(name)
        .and_then(|child| child.get_text())
        .map(|t| t.trim().to_owned())
        .unwrap_or_default()
}
fn set(element: &mut Element, name: &str, value: impl ToString) {
    let value = value.to_string();
    if let Some(child) = element.get_mut_child(name) {
        child.children = vec![XMLNode::Text(value)];
    } else {
        let mut child = Element::new(name);
        child.children.push(XMLNode::Text(value));
        element.children.push(XMLNode::Element(child));
    }
}
fn profiles(root: &mut Element) -> Result<&mut Element> {
    root.get_mut_child("GameProfiles")
        .context("Lossless Scaling settings have no game profiles")
}
fn remove_ours(profiles: &mut Element) -> bool {
    let before = profiles.children.len();
    profiles.children.retain(|node| match node {
        XMLNode::Element(profile) => {
            !(profile.name == "Profile" && text_of(profile, "Title") == TITLE)
        }
        _ => true,
    });
    profiles.children.len() != before
}
fn number(value: f64) -> String {
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}
/// Settings with our profile for the programs in `filter` (lower-case file
/// names joined with `;`), copied from the default profile.
pub fn with_profile(settings: &str, profile: &Profile, filter: &str) -> Result<String> {
    let mut root = parse(settings)?;
    let list = profiles(&mut root)?;
    remove_ours(list);
    let template = {
        let profiles: Vec<&Element> = list
            .children
            .iter()
            .filter_map(XMLNode::as_element)
            .filter(|p| p.name == "Profile")
            .collect();
        profiles
            .iter()
            .find(|p| text_of(p, "Path").is_empty())
            .or(profiles.first())
            .map(|p| (*p).clone())
            .unwrap_or_else(|| Element::new("Profile"))
    };
    let mut ours = template;
    set(&mut ours, "Title", TITLE);
    set(&mut ours, "Path", filter);
    set(&mut ours, "Filter", filter);
    set(&mut ours, "AutoScale", profile.auto_scale);
    set(&mut ours, "AutoScaleDelay", 0);
    set(&mut ours, "SyncMode", "OFF");
    if let Some(capture) = profile.capture_api {
        set(&mut ours, "CaptureApi", capture);
    }
    if let Some(queue) = profile.queue_target {
        set(&mut ours, "QueueTarget", queue.max(0));
    }
    if let Some(hdr) = profile.hdr {
        set(&mut ours, "HdrSupport", hdr);
    }
    set(&mut ours, "FrameGeneration", profile.frame_generation);
    if let Some(mode) = profile.lsfg3_mode {
        set(&mut ours, "LSFG3Mode1", mode);
    }
    let size = if profile.performance_mode {
        "PERFORMANCE"
    } else {
        "BALANCED"
    };
    set(&mut ours, "LSFGSize", size);
    if profile.scaling_type == "LS1" {
        set(&mut ours, "LS1Type", size);
    }
    set(&mut ours, "MaxFrameLatency", 1);
    set(&mut ours, "LSFGFlowScale", profile.flow_scale);
    if let Some(target) = profile.target_fps {
        set(&mut ours, "LSFG3Target", number(target));
    }
    set(&mut ours, "ScaleFactor", number(profile.scale_factor));
    set(&mut ours, "ScalingType", profile.scaling_type);
    if (profile.scale_factor - 1.).abs() > 0.01 {
        set(&mut ours, "ScalingMode", "Custom");
        set(&mut ours, "ResizeBeforeScaling", true);
    }
    if let Some(sharpness) = profile.sharpness {
        set(&mut ours, "Sharpness", sharpness);
    }
    if let Some(sharpness) = profile.ls1_sharpness {
        set(&mut ours, "LS1Sharpness", sharpness);
    }
    if let Some(kind) = &profile.anime4k_type {
        set(&mut ours, "Anime4kType", kind);
    }
    if let Some(vrs) = profile.anime4k_vrs {
        set(&mut ours, "VRS", vrs);
    }
    list.children.push(XMLNode::Element(ours));
    write(&root)
}
/// Settings without our profile, or `None` when there is none.
pub fn without_profile(settings: &str) -> Result<Option<String>> {
    let mut root = parse(settings)?;
    if !remove_ours(profiles(&mut root)?) {
        return Ok(None);
    }
    write(&root).map(Some)
}
/// The hotkey Lossless Scaling scales with.
pub fn hotkey(settings: &str) -> Option<(Vec<u16>, u16)> {
    let root = parse(settings).ok()?;
    butterpollo_core::lossless::hotkey(
        &text_of(&root, "Hotkey"),
        &text_of(&root, "HotkeyModifierKeys"),
    )
}
/// The game's program names: the programs in `folder` (up to 256), or the
/// programs that are running.
fn filter(folder: Option<&Path>, running: &[String]) -> String {
    let mut names: BTreeSet<String> = running.iter().map(|n| n.to_ascii_lowercase()).collect();
    if let Some(folder) = folder {
        let mut pending = vec![(folder.to_owned(), 0)];
        while let Some((dir, depth)) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && depth < 6 {
                    pending.push((path, depth + 1));
                } else if path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
                    && let Some(name) = path.file_name()
                {
                    names.insert(name.to_string_lossy().to_ascii_lowercase());
                }
                if names.len() >= 256 {
                    break;
                }
            }
        }
    }
    names.into_iter().collect::<Vec<_>>().join(";")
}

const SHELLS: &[&str] = &[
    "cmd.exe",
    "conhost.exe",
    "explorer.exe",
    "steam.exe",
    "steamwebhelper.exe",
    "playnite.desktopapp.exe",
    "playnite.fullscreenapp.exe",
    "werfault.exe",
    "losslessscaling.exe",
    "lossless scaling.exe",
];
/// Lossless Scaling for one running app.
pub struct Session {
    stop: Arc<AtomicBool>,
    applied: Arc<Mutex<Option<PathBuf>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Session {
    /// `folder` names the game's folder once it is known (it may change).
    pub fn start(
        options: Options,
        configured_program: String,
        baseline: Vec<butterpollo_core::steam::Process>,
        folder: Box<dyn Fn() -> Option<String> + Send>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let applied = Arc::new(Mutex::new(None));
        let worker = {
            let (stop, applied) = (stop.clone(), applied.clone());
            std::thread::Builder::new()
                .name("lossless-scaling".into())
                .spawn(move || {
                    if let Err(error) = run(&options, &configured_program, &baseline, folder.as_ref(), &stop, &applied) {
                        tracing::warn!(error = %format!("{error:#}"), "Lossless Scaling was not started");
                    }
                })
                .ok()
        };
        Self {
            stop,
            applied,
            worker,
        }
    }
    /// Close Lossless Scaling and remove the profile.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let Some(settings) = self.applied.lock().unwrap().take() else {
            return;
        };
        close_lossless();
        let restored = std::fs::read_to_string(&settings)
            .map_err(anyhow::Error::from)
            .and_then(|text| without_profile(&text))
            .and_then(|text| match text {
                Some(text) => butterpollo_core::state::atomic_write(&settings, text.as_bytes()),
                None => Ok(()),
            });
        match restored {
            Ok(()) => tracing::info!("closed Lossless Scaling and removed the stream's profile"),
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "the Lossless Scaling profile could not be removed")
            }
        }
    }
}
fn lossless_processes() -> std::collections::BTreeMap<u32, u64> {
    butterpollo_windows::process::processes()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| {
            butterpollo_windows::lossless::PROCESSES
                .iter()
                .any(|n| p.name.eq_ignore_ascii_case(n))
        })
        .map(|p| (p.pid, p.started))
        .collect()
}
fn close_lossless() {
    butterpollo_windows::process::stop_processes(&lossless_processes(), Duration::from_secs(4));
}
fn run(
    options: &Options,
    configured_program: &str,
    baseline: &[butterpollo_core::steam::Process],
    folder: &(dyn Fn() -> Option<String> + Send),
    stop: &AtomicBool,
    applied: &Mutex<Option<PathBuf>>,
) -> Result<()> {
    let wait = |duration: Duration| {
        let until = Instant::now() + duration;
        while Instant::now() < until {
            if stop.load(Ordering::Acquire) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        true
    };
    let program = butterpollo_windows::lossless::program(configured_program)
        .context("Lossless Scaling was not found; set its path in Settings")?;
    let settings = butterpollo_windows::lossless::settings_path().context("no signed-in user")?;
    // The game: a new process with a window, inside the game's folder when
    // that is known.
    let known: HashSet<(u32, u64)> = baseline.iter().map(|p| (p.pid, p.started)).collect();
    let deadline = Instant::now() + Duration::from_secs(120);
    let (game, folder) = loop {
        if !wait(Duration::from_secs(1)) {
            return Ok(());
        }
        let folder = folder().filter(|f| !f.trim().is_empty());
        let inside = |pid: u32| {
            folder.as_deref().is_none_or(|f| {
                butterpollo_windows::process::image_path(pid).is_some_and(|path| {
                    path.to_ascii_lowercase().starts_with(
                        &f.replace('/', "\\")
                            .trim_end_matches('\\')
                            .to_ascii_lowercase(),
                    )
                })
            })
        };
        let candidates: Vec<butterpollo_core::steam::Process> =
            butterpollo_windows::process::processes()?
                .into_iter()
                .filter(|p| !known.contains(&(p.pid, p.started)))
                .filter(|p| !SHELLS.contains(&p.name.to_ascii_lowercase().as_str()))
                .filter(|p| !butterpollo_windows::lossless::windows_of(p.pid).is_empty())
                .filter(|p| inside(p.pid))
                .collect();
        if let Some(game) = candidates.into_iter().next() {
            break (game, folder);
        }
        if Instant::now() > deadline {
            bail!("no game window appeared");
        }
    };
    tracing::info!(game = %game.name, delay = options.launch_delay, "starting Lossless Scaling for the game");
    if !wait(Duration::from_secs(options.launch_delay)) {
        return Ok(());
    }
    close_lossless();
    let text = std::fs::read_to_string(&settings)
        .with_context(|| format!("reading {}", settings.display()))?;
    let updated = with_profile(
        &text,
        &options.profile,
        &filter(
            folder.as_deref().map(Path::new),
            std::slice::from_ref(&game.name),
        ),
    )?;
    butterpollo_core::state::atomic_write(&settings, updated.as_bytes())?;
    *applied.lock().unwrap() = Some(settings.clone());
    butterpollo_windows::process::Process::spawn_detached(
        &program,
        &[],
        butterpollo_windows::process::Target::User { elevated: false },
    )?;
    let started = Instant::now();
    let lossless = loop {
        if let Some(pid) = lossless_processes().keys().next().copied()
            && !butterpollo_windows::lossless::windows_of(pid).is_empty()
        {
            break Some(pid);
        }
        if started.elapsed() > Duration::from_secs(10) || !wait(Duration::from_millis(200)) {
            break None;
        }
    };
    if options.legacy_auto_detect {
        // Lossless Scaling scales the game by itself (AutoScale).
        return Ok(());
    }
    if let Some(pid) = lossless {
        butterpollo_windows::lossless::minimize(pid);
    }
    let Some((modifiers, key)) = hotkey(&updated) else {
        bail!("Lossless Scaling has no usable hotkey");
    };
    for attempt in 1..=3 {
        let focused = butterpollo_windows::lossless::focus(game.pid);
        wait(Duration::from_millis(250));
        let sent = butterpollo_windows::lossless::press(&modifiers, key);
        tracing::info!(attempt, focused, sent, "sent the Lossless Scaling hotkey");
        if focused && sent {
            break;
        }
        if !wait(Duration::from_millis(500)) {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const SETTINGS: &str = "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Settings xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\n  <Hotkey>S</Hotkey>\n  <HotkeyModifierKeys>Alt Control</HotkeyModifierKeys>\n  <GameProfiles>\n    <Profile>\n      <Title>Default</Title>\n      <ScalingType>Off</ScalingType>\n      <CaptureApi>DXGI</CaptureApi>\n    </Profile>\n    <Profile>\n      <Title>Elden Ring</Title>\n      <Path>eldenring.exe</Path>\n    </Profile>\n    <Profile>\n      <Title>Vibeshine</Title>\n      <Path>old.exe</Path>\n    </Profile>\n  </GameProfiles>\n</Settings>";
    #[test]
    fn the_profile_is_added_from_the_default_and_removed_again() {
        let options = butterpollo_core::lossless::options(
            &serde_json::json!({"frame-generation-mode": "lossless-scaling", "lossless-scaling-profile": "recommended"}),
            &butterpollo_core::config::Config::default(),
            120.,
        )
        .unwrap();
        let updated = with_profile(SETTINGS, &options.profile, "game.exe;launcher.exe").unwrap();
        let root = parse(&updated).unwrap();
        let list = root.get_child("GameProfiles").unwrap();
        let titles: Vec<String> = list
            .children
            .iter()
            .filter_map(XMLNode::as_element)
            .map(|p| text_of(p, "Title"))
            .collect();
        assert_eq!(titles, ["Default", "Elden Ring", "Vibeshine"]);
        let ours = list
            .children
            .iter()
            .rev()
            .find_map(XMLNode::as_element)
            .unwrap();
        assert_eq!(text_of(ours, "Path"), "game.exe;launcher.exe");
        assert_eq!(text_of(ours, "CaptureApi"), "WGC");
        assert_eq!(text_of(ours, "FrameGeneration"), "LSFG3");
        assert_eq!(text_of(ours, "LSFG3Target"), "120");
        assert_eq!(text_of(ours, "AutoScale"), "false");
        assert_eq!(text_of(ours, "ScalingType"), "Off");
        assert!(updated.contains("xmlns:xsi"));
        assert_eq!(hotkey(&updated), Some((vec![0x11, 0x12], u16::from(b'S'))));
        let restored = without_profile(&updated).unwrap().unwrap();
        assert!(!restored.contains("Vibeshine") && restored.contains("Elden Ring"));
        assert_eq!(without_profile(&restored).unwrap(), None);
    }
}
