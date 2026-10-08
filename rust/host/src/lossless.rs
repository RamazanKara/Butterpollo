//! Lossless Scaling for a stream, as in Vibepollo: once the game runs, a
//! temporary "Vibeshine" game profile limited to the game's programs is
//! added to Lossless Scaling's settings, Lossless Scaling is restarted, and
//! its hotkey starts scaling the game. When the app stops, Lossless Scaling
//! is closed and the profile removed.
use anyhow::{Context, Result, bail};
use butterpollo_core::lossless::{Options, Profile};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
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

const OBSERVATION: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
struct Sample {
    process: butterpollo_core::steam::Process,
    path: String,
    cpu_time: u64,
    working_set: u64,
    windowed: bool,
}
impl Sample {
    fn identity(&self) -> (u32, u64) {
        (self.process.pid, self.process.started)
    }
}
struct Candidate {
    sample: Sample,
    start_cpu: u64,
    peak_working_set: u64,
    first_seen: Duration,
    last_seen: Duration,
}
fn normalized(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}
fn in_folder(path: &str, folder: Option<&str>) -> bool {
    let Some(folder) = folder.filter(|folder| !folder.is_empty()) else {
        return false;
    };
    let folder = normalized(folder);
    let folder = folder.trim_end_matches('\\');
    let path = normalized(path);
    path == folder
        || path
            .strip_prefix(folder)
            .is_some_and(|tail| tail.starts_with('\\'))
}
fn eligible(
    sample: &Sample,
    baseline: &HashSet<(u32, u64)>,
    current: Option<&Sample>,
    folder: Option<&str>,
) -> bool {
    if current.is_none() {
        return !baseline.contains(&sample.identity());
    }
    let ignored = [
        "losslessscaling.exe",
        "lossless scaling.exe",
        "playnite.desktopapp.exe",
        "playnite.fullscreenapp.exe",
    ];
    sample.windowed
        && !ignored
            .iter()
            .any(|name| sample.process.name.eq_ignore_ascii_case(name))
        && (in_folder(&sample.path, folder)
            || current.is_some_and(|game| normalized(&sample.path) == normalized(&game.path)))
}
fn observe(candidates: &mut BTreeMap<(u32, u64), Candidate>, samples: Vec<Sample>, now: Duration) {
    let alive: HashSet<_> = samples.iter().map(Sample::identity).collect();
    // An exited launcher must not beat its replacement; creation times also distinguish PID reuse.
    candidates.retain(|identity, _| alive.contains(identity));
    for sample in samples {
        let candidate = candidates
            .entry(sample.identity())
            .or_insert_with(|| Candidate {
                start_cpu: sample.cpu_time,
                peak_working_set: 0,
                first_seen: now,
                last_seen: now,
                sample: sample.clone(),
            });
        if candidate.start_cpu == 0 {
            candidate.start_cpu = sample.cpu_time;
        }
        candidate.peak_working_set = candidate.peak_working_set.max(sample.working_set);
        candidate.last_seen = now;
        candidate.sample = sample;
    }
}
fn select_game<'a>(
    candidates: &'a BTreeMap<(u32, u64), Candidate>,
    root_pid: u32,
    folder: Option<&str>,
    executable: Option<&str>,
    windows: Option<&str>,
    cpu_count: u32,
) -> Option<&'a Sample> {
    let scores: Vec<_> = candidates
        .values()
        .filter_map(|candidate| {
            if candidate.start_cpu == 0
                || candidate.sample.cpu_time < candidate.start_cpu
                || candidate.last_seen <= candidate.first_seen
                || candidate.sample.path.is_empty()
            {
                return None;
            }
            let elapsed = (candidate.last_seen - candidate.first_seen)
                .as_secs_f64()
                .max(0.1);
            let cpu = (candidate.sample.cpu_time - candidate.start_cpu) as f64
                / 10_000_000.
                / (elapsed * f64::from(cpu_count));
            let memory = candidate.peak_working_set as f64 / (1024. * 1024.);
            Some((&candidate.sample, cpu, memory))
        })
        .collect();
    let max_cpu = scores.iter().map(|(_, cpu, _)| *cpu).fold(0., f64::max);
    let max_memory = scores
        .iter()
        .map(|(_, _, memory)| *memory)
        .fold(0., f64::max);
    let cpu_weight = if max_cpu < 0.08 { 0.5 } else { 0.7 };
    let mut best = None;
    let mut best_score = -1.;
    for (sample, cpu, memory) in scores {
        let cpu_norm = if max_cpu > 0. { cpu / max_cpu } else { 0. };
        let memory_norm = if max_memory > 0. {
            memory / max_memory
        } else {
            0.
        };
        let preferred = in_folder(&sample.path, folder);
        let mut score = cpu_weight * cpu_norm + (1. - cpu_weight) * memory_norm;
        if preferred {
            score += 0.2;
        }
        if executable.is_some_and(|exe| normalized(exe) == normalized(&sample.path)) {
            score += 0.25;
        }
        if sample.process.pid == root_pid {
            score += if preferred { 0.05 } else { -0.05 };
        }
        score += cpu.min(1.) * 0.15;
        if windows.is_some_and(|path| !path.is_empty()) {
            if in_folder(&sample.path, windows) {
                score -= 0.2;
                if cpu < 0.02 && memory < 48. {
                    score -= 0.05;
                }
            }
        } else if cpu < 0.015 && memory < 32. {
            score -= 0.05;
        }
        if score > best_score {
            best_score = score;
            best = Some(sample);
        }
    }
    best
}
fn retarget<'a>(current: Option<&Sample>, selected: Option<&'a Sample>) -> Option<&'a Sample> {
    selected.filter(|next| current.is_none_or(|game| game.identity() != next.identity()))
}
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
        root_pid: u32,
        folder: Box<dyn Fn() -> Option<String> + Send>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let applied = Arc::new(Mutex::new(None));
        let worker = {
            let (stop, applied) = (stop.clone(), applied.clone());
            std::thread::Builder::new()
                .name("lossless-scaling".into())
                .spawn(move || {
                    if let Err(error) = run(&options, &configured_program, &baseline, root_pid, folder.as_ref(), &stop, &applied) {
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
fn wait(stop: &AtomicBool, duration: Duration) -> bool {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        std::thread::sleep(
            until
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(100)),
        );
    }
    !stop.load(Ordering::Acquire)
}
fn run(
    options: &Options,
    configured_program: &str,
    baseline: &[butterpollo_core::steam::Process],
    root_pid: u32,
    folder: &(dyn Fn() -> Option<String> + Send),
    stop: &AtomicBool,
    applied: &Mutex<Option<PathBuf>>,
) -> Result<()> {
    let program = butterpollo_windows::lossless::program(configured_program)
        .context("Lossless Scaling was not found; set its path in Settings")?;
    let settings = butterpollo_windows::lossless::settings_path().context("no signed-in user")?;
    let known: HashSet<(u32, u64)> = baseline.iter().map(|p| (p.pid, p.started)).collect();
    let (cpu_count, windows) = butterpollo_windows::lossless::scoring_environment();
    let mut current: Option<Sample> = None;
    loop {
        let folder = folder().filter(|f| !f.trim().is_empty()).or_else(|| {
            current
                .as_ref()
                .and_then(|game| Path::new(&game.path).parent())
                .map(|path| path.to_string_lossy().into_owned())
        });
        let started = Instant::now();
        let mut candidates = BTreeMap::new();
        while started.elapsed() < OBSERVATION {
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            let samples = butterpollo_windows::process::processes()?
                .into_iter()
                .filter(|p| current.is_some() || !known.contains(&(p.pid, p.started)))
                .filter_map(|process| {
                    let path = butterpollo_windows::process::image_path(process.pid)?;
                    let mut sample = Sample {
                        windowed: true,
                        process,
                        path,
                        cpu_time: 0,
                        working_set: 0,
                    };
                    if !eligible(&sample, &known, current.as_ref(), folder.as_deref()) {
                        return None;
                    }
                    if current.is_some() {
                        sample.windowed =
                            !butterpollo_windows::lossless::windows_of(sample.process.pid)
                                .is_empty();
                        if !sample.windowed {
                            return None;
                        }
                    }
                    let usage = butterpollo_windows::process::usage(sample.process.pid)?;
                    if usage.started != sample.process.started {
                        return None;
                    }
                    sample.cpu_time = usage.cpu_time;
                    sample.working_set = usage.working_set;
                    Some(sample)
                })
                .collect();
            observe(&mut candidates, samples, started.elapsed());
            if !wait(stop, POLL) {
                return Ok(());
            }
        }
        let selected = select_game(
            &candidates,
            current.as_ref().map_or(root_pid, |game| game.process.pid),
            folder.as_deref(),
            current.as_ref().map(|game| game.path.as_str()),
            windows.as_deref(),
            cpu_count,
        )
        .or_else(|| {
            // The focus selector can still use a window when no CPU samples were usable.
            current
                .as_ref()
                .and_then(|_| candidates.values().next().map(|c| &c.sample))
        });
        let Some(game) = retarget(current.as_ref(), selected).cloned() else {
            continue;
        };
        if current.is_none() && !wait(stop, Duration::from_secs(options.launch_delay)) {
            return Ok(());
        }
        if butterpollo_windows::process::creation_time(game.process.pid) != game.process.started {
            continue;
        }
        tracing::info!(game = %game.process.name, pid = game.process.pid, "targeting Lossless Scaling at the game");
        apply_game(
            options,
            &program,
            &settings,
            &game,
            folder.as_deref(),
            stop,
            applied,
        )?;
        current = Some(game);
    }
}
fn apply_game(
    options: &Options,
    program: &Path,
    settings: &Path,
    game: &Sample,
    folder: Option<&str>,
    stop: &AtomicBool,
    applied: &Mutex<Option<PathBuf>>,
) -> Result<()> {
    close_lossless();
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let text = std::fs::read_to_string(settings)
        .with_context(|| format!("reading {}", settings.display()))?;
    let updated = with_profile(
        &text,
        &options.profile,
        &filter(
            folder
                .map(Path::new)
                .or_else(|| Path::new(&game.path).parent()),
            std::slice::from_ref(&game.process.name),
        ),
    )?;
    butterpollo_core::state::atomic_write(settings, updated.as_bytes())?;
    *applied.lock().unwrap() = Some(settings.to_owned());
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    butterpollo_windows::process::Process::spawn_detached(
        program,
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
        if started.elapsed() > Duration::from_secs(10) || !wait(stop, Duration::from_millis(200)) {
            break None;
        }
    };
    if options.legacy_auto_detect || stop.load(Ordering::Acquire) {
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
        let focused = butterpollo_windows::lossless::focus(game.process.pid);
        if !wait(stop, Duration::from_millis(250)) {
            break;
        }
        let sent = butterpollo_windows::lossless::press(&modifiers, key);
        tracing::info!(attempt, focused, sent, "sent the Lossless Scaling hotkey");
        if focused && sent {
            break;
        }
        if !wait(stop, Duration::from_millis(500)) {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(pid: u32, path: &str, cpu_time: u64, memory_mb: u64) -> Sample {
        Sample {
            process: butterpollo_core::steam::Process {
                pid,
                parent: 0,
                started: 1,
                name: path.rsplit(['/', '\\']).next().unwrap().into(),
            },
            path: path.into(),
            cpu_time,
            working_set: memory_mb * 1024 * 1024,
            windowed: true,
        }
    }
    fn observed(samples: Vec<Sample>) -> BTreeMap<(u32, u64), Candidate> {
        let mut candidates = BTreeMap::new();
        let first = samples
            .iter()
            .cloned()
            .map(|mut sample| {
                sample.cpu_time = 1;
                sample
            })
            .collect();
        observe(&mut candidates, first, Duration::ZERO);
        observe(&mut candidates, samples, OBSERVATION);
        candidates
    }
    #[test]
    fn scoring_switches_from_equal_cpu_memory_weights_at_eight_percent() {
        let mut processes = vec![
            sample(1, r"C:\Games\launcher.exe", 16_000_001, 100),
            sample(2, r"C:\Games\game.exe", 31_600_001, 10),
        ];
        let candidates = observed(processes.clone());
        assert_eq!(
            select_game(&candidates, 0, None, None, Some(r"C:\Windows"), 4)
                .unwrap()
                .process
                .pid,
            1
        );
        processes[1].cpu_time = 32_000_001;
        let candidates = observed(processes);
        assert_eq!(
            select_game(&candidates, 0, None, None, Some(r"C:\Windows"), 4)
                .unwrap()
                .process
                .pid,
            2
        );
    }
    #[test]
    fn scoring_uses_peak_memory_and_each_processes_observed_lifetime() {
        let mut candidates = BTreeMap::new();
        observe(
            &mut candidates,
            vec![sample(1, r"C:\game.exe", 1, 1000)],
            Duration::ZERO,
        );
        observe(
            &mut candidates,
            vec![
                sample(1, r"C:\game.exe", 1, 1),
                sample(2, r"C:\launcher.exe", 1, 100),
            ],
            Duration::from_secs(8),
        );
        observe(
            &mut candidates,
            vec![
                sample(1, r"C:\game.exe", 1, 1),
                sample(2, r"C:\launcher.exe", 1, 100),
            ],
            OBSERVATION,
        );
        assert_eq!(
            select_game(&candidates, 0, None, None, None, 1)
                .unwrap()
                .process
                .pid,
            1
        );
        // The new game consumes less total CPU, but twice as much CPU per second observed.
        observe(
            &mut candidates,
            vec![
                sample(1, r"C:\game.exe", 10_000_001, 1),
                sample(2, r"C:\launcher.exe", 4_000_001, 100),
            ],
            OBSERVATION,
        );
        assert_eq!(
            select_game(&candidates, 0, None, None, None, 1)
                .unwrap()
                .process
                .pid,
            2
        );
    }
    #[test]
    fn folder_root_and_executable_bonuses_match_vibepollo() {
        let candidates = observed(vec![
            sample(1, r"C:\Games\Game2\launcher.exe", 10_000_001, 100),
            sample(2, r"C:\Games\Game\game.exe", 10_000_001, 100),
        ]);
        assert_eq!(
            select_game(&candidates, 1, None, None, None, 1)
                .unwrap()
                .process
                .pid,
            2
        );
        assert_eq!(
            select_game(&candidates, 0, Some("c:/GAMES/game/"), None, None, 1)
                .unwrap()
                .process
                .pid,
            2
        );
        assert_eq!(
            select_game(&candidates, 1, Some(r"C:\Games"), None, None, 1)
                .unwrap()
                .process
                .pid,
            1
        );
        assert_eq!(
            select_game(
                &candidates,
                1,
                Some(r"C:\Games"),
                Some("c:/games/game/GAME.exe"),
                None,
                1
            )
            .unwrap()
            .process
            .pid,
            2
        );
        assert!(!in_folder(
            r"C:\Games\Game2\game.exe",
            Some(r"C:\Games\Game")
        ));
    }
    #[test]
    fn windows_processes_and_small_idle_processes_are_penalized() {
        let candidates = observed(vec![
            sample(1, r"C:\Windows\helper.exe", 1_000_001, 40),
            sample(2, r"C:\Game\game.exe", 1_000_001, 40),
        ]);
        // The system penalty outweighs the executable bonus for a small idle process.
        assert_eq!(
            select_game(
                &candidates,
                1,
                None,
                Some(r"C:\Windows\helper.exe"),
                Some("c:/windows/"),
                1
            )
            .unwrap()
            .process
            .pid,
            2
        );
        let candidates = observed(vec![
            sample(1, r"C:\Game\small.exe", 1_000_001, 31),
            sample(2, r"C:\Game\game.exe", 1_000_001, 32),
        ]);
        assert_eq!(
            select_game(&candidates, 0, Some(r"C:\Game\small.exe"), None, None, 1)
                .unwrap()
                .process
                .pid,
            1
        );
        assert_eq!(
            select_game(&candidates, 0, None, None, None, 1)
                .unwrap()
                .process
                .pid,
            2
        );
    }
    #[test]
    fn invalid_or_single_samples_are_not_scored() {
        let mut candidates = observed(vec![
            sample(1, "", 100, 100),
            sample(2, r"C:\zero.exe", 0, 100),
        ]);
        let backwards = candidates.get_mut(&(2, 1)).unwrap();
        backwards.start_cpu = 100;
        assert!(select_game(&candidates, 0, None, None, None, 1).is_none());
        let mut candidates = BTreeMap::new();
        observe(
            &mut candidates,
            vec![sample(1, r"C:\game.exe", 100, 100)],
            Duration::ZERO,
        );
        assert!(select_game(&candidates, 0, None, None, None, 1).is_none());
    }
    #[test]
    fn bootstrap_excludes_the_baseline_and_focus_requires_matching_game_windows() {
        let game = sample(1, r"C:\Game\game.exe", 1, 100);
        let baseline = HashSet::from([game.identity()]);
        assert!(!eligible(&game, &baseline, None, None));
        let mut restarted = game.clone();
        restarted.process.started += 1;
        assert!(eligible(&restarted, &baseline, None, None));
        assert!(eligible(&game, &baseline, Some(&game), None));
        for name in [
            "LosslessScaling.exe",
            "Lossless Scaling.exe",
            "Playnite.DesktopApp.exe",
            "Playnite.FullscreenApp.exe",
        ] {
            assert!(!eligible(
                &sample(2, &format!(r"C:\Game\{name}"), 1, 100),
                &baseline,
                Some(&game),
                Some(r"C:\Game")
            ));
        }
        assert!(!eligible(
            &sample(2, r"C:\Game2\unrelated.exe", 1, 100),
            &baseline,
            Some(&game),
            Some(r"C:\Game")
        ));
        restarted.windowed = false;
        assert!(!eligible(
            &restarted,
            &baseline,
            Some(&game),
            Some(r"C:\Game")
        ));
    }
    #[test]
    fn retargets_a_scored_launcher_replacement_and_restarts_but_not_an_unchanged_game() {
        let launcher = sample(1, r"C:\Game\launcher.exe", 1_000_001, 10);
        let game = sample(2, r"C:\Game\game.exe", 80_000_001, 1000);
        let mut candidates = observed(vec![launcher.clone(), game.clone()]);
        let selected = select_game(
            &candidates,
            1,
            Some(r"C:\Game"),
            Some(&launcher.path),
            None,
            1,
        );
        assert_eq!(
            retarget(Some(&launcher), selected).unwrap().identity(),
            game.identity()
        );
        assert!(retarget(Some(&game), selected).is_none());
        assert_eq!(
            retarget(None, selected).unwrap().identity(),
            game.identity()
        );
        observe(&mut candidates, vec![], OBSERVATION + POLL);
        assert!(
            retarget(
                Some(&game),
                select_game(&candidates, 0, None, None, None, 1)
            )
            .is_none()
        );
        let mut restarted = game.clone();
        restarted.process.started += 1;
        candidates = observed(vec![restarted.clone()]);
        let selected = select_game(&candidates, 2, Some(r"C:\Game"), Some(&game.path), None, 1);
        assert_eq!(
            retarget(Some(&game), selected).unwrap().identity(),
            restarted.identity()
        );
    }
    #[test]
    fn dead_launchers_and_reused_pids_do_not_keep_their_old_scores() {
        let mut candidates = observed(vec![sample(1, r"C:\Game\launcher.exe", 100_000_001, 1000)]);
        let mut game = sample(1, r"C:\Game\game.exe", 1, 10);
        game.process.started = 2;
        observe(&mut candidates, vec![game.clone()], OBSERVATION + POLL);
        assert_eq!(candidates.len(), 1);
        assert!(select_game(&candidates, 0, None, None, None, 1).is_none());
        game.cpu_time += 1_000_000;
        observe(&mut candidates, vec![game.clone()], OBSERVATION + POLL * 2);
        assert_eq!(
            select_game(&candidates, 0, None, None, None, 1)
                .unwrap()
                .identity(),
            game.identity()
        );
    }
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
