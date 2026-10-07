//! Playnite, as in Vibepollo: the plugin's games become apps, and an app
//! linked to a Playnite game is started through Playnite with the stream's
//! environment, ending when Playnite reports the game stopped.
use crate::state::Shared;
use anyhow::{Context, Result, bail};
use butterpollo_core::playnite::{self, Artwork, Game, Message, Settings};
use butterpollo_windows::playnite::Pipe;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

const PLUGIN_FILES: [&str; 2] = ["extension.yaml", "SunshinePlaynite.psm1"];

fn now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
/// The plugin shipped with this installation.
pub fn packaged_plugin(h: &Shared) -> PathBuf {
    h.assets
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&h.assets)
        .join("plugins")
        .join("playnite")
        .join("SunshinePlaynite")
}
fn version(folder: &Path) -> Option<String> {
    playnite::plugin_version(&std::fs::read_to_string(folder.join("extension.yaml")).ok()?)
}
fn installed_plugin() -> Option<PathBuf> {
    butterpollo_windows::playnite::extensions_dir().map(|dir| dir.join("SunshinePlaynite"))
}
/// Copy the packaged plugin into Playnite's extensions. Playnite loads it
/// when it next starts.
pub fn install_plugin(h: &Shared) -> Result<PathBuf> {
    let source = packaged_plugin(h);
    if !PLUGIN_FILES.iter().all(|file| source.join(file).is_file()) {
        bail!("this installation has no Playnite plugin");
    }
    let target = installed_plugin().context("Playnite was not found on this PC")?;
    std::fs::create_dir_all(&target)?;
    for file in PLUGIN_FILES {
        butterpollo_core::state::atomic_write(
            &target.join(file),
            &std::fs::read(source.join(file))?,
        )?;
    }
    tracing::info!(folder = %target.display(), "installed the Playnite plugin");
    Ok(target)
}
/// Install the packaged plugin over an older one.
fn update_plugin(h: &Shared) {
    let (Some(target), Some(packaged)) = (installed_plugin(), version(&packaged_plugin(h))) else {
        return;
    };
    if let Some(installed) = version(&target)
        && playnite::newer(&packaged, &installed)
        && let Err(error) = install_plugin(h)
    {
        tracing::warn!(error = %format!("{error:#}"), "the Playnite plugin could not be updated");
    }
}

/// What the plugin last reported.
#[derive(Default)]
struct Catalog {
    games: Vec<Game>,
    categories: Vec<(String, String)>,
    plugins: Vec<(String, String)>,
    at: Option<Instant>,
}
static CATALOG: Mutex<Catalog> = Mutex::new(Catalog {
    games: vec![],
    categories: vec![],
    plugins: vec![],
    at: None,
});
/// Ask the plugin for its library.
fn snapshot() -> Result<()> {
    let pipe = Pipe::connect(&json!({"type":"hello","role":"sunshine","pid":std::process::id()}))?;
    let mut games = vec![];
    let (mut categories, mut plugins) = (vec![], vec![]);
    let (mut started, mut asked, mut complete) = (false, false, false);
    let opened = Instant::now();
    let mut last = Instant::now();
    while !complete {
        match pipe.lines.recv_timeout(Duration::from_millis(250)) {
            Ok(line) => {
                last = Instant::now();
                match playnite::parse(&line) {
                    Message::SnapshotStart => {
                        started = true;
                        games.clear();
                    }
                    Message::SnapshotComplete => complete = true,
                    Message::Games(batch) => games.extend(batch),
                    Message::Categories(list) => categories = list,
                    Message::Plugins(list) => plugins = list,
                    _ => {}
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                bail!("the Playnite plugin closed the connection")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        // The plugin sends its library on connecting; ask if it has not.
        if !started && !asked && opened.elapsed() > Duration::from_secs(3) {
            pipe.send(&json!({"type":"command","command":"snapshot"}))?;
            asked = true;
        }
        // Older plugins send no end marker.
        if !started && !games.is_empty() && last.elapsed() > Duration::from_secs(3) {
            complete = true;
        }
        if opened.elapsed() > Duration::from_secs(60) {
            bail!("the Playnite library did not arrive in time");
        }
    }
    *CATALOG.lock().unwrap() = Catalog {
        games,
        categories,
        plugins,
        at: Some(Instant::now()),
    };
    Ok(())
}
/// A PNG of `source` in the covers folder, converted again only when the
/// source changes.
fn png(h: &Shared, source: &str, name: &str) -> Option<PathBuf> {
    let source = Path::new(source);
    let metadata = std::fs::metadata(source).ok()?;
    let folder = h.directory.join("covers");
    let png = folder.join(format!("{name}.png"));
    let stamp = format!(
        "{}|{}|{:?}",
        source.display(),
        metadata.len(),
        metadata.modified().ok()
    );
    let record = folder.join(format!("{name}.png.src"));
    if png.is_file() && std::fs::read_to_string(&record).is_ok_and(|saved| saved == stamp) {
        return Some(png);
    }
    match butterpollo_windows::image::to_png(source, &png) {
        Ok(_) => {
            let _ = butterpollo_core::state::atomic_write(&record, stamp.as_bytes());
            Some(png)
        }
        Err(error) => {
            tracing::debug!(error = %format!("{error:#}"), source = %source.display(), "Playnite artwork conversion failed");
            png.is_file().then_some(png)
        }
    }
}
pub struct Outcome {
    pub changed: bool,
    pub games: usize,
}
/// Fetch Playnite's library and bring the apps in line with it.
pub fn sync(h: &Shared) -> Result<Outcome> {
    let settings = Settings::from_config(&h.config.read().unwrap());
    if !settings.enabled {
        bail!("Playnite integration is disabled");
    }
    if butterpollo_windows::playnite::running().is_none() {
        bail!("Playnite is not running");
    }
    snapshot()?;
    let games = CATALOG.lock().unwrap().games.clone();
    let artwork: Artwork = games
        .iter()
        .map(|game| {
            let id = game.id.to_ascii_lowercase();
            let cover = (!game.box_art_path.is_empty())
                .then(|| png(h, &game.box_art_path, &format!("playnite_{id}")))
                .flatten();
            let icon = (!game.icon_path.is_empty())
                .then(|| png(h, &game.icon_path, &format!("playnite_icon_{id}")))
                .flatten();
            (id, (cover, icon))
        })
        .collect();
    let changed = crate::steam::update_apps(h, |apps| {
        let synced =
            playnite::reconcile(apps, &games, &settings, now(), &artwork, settings.auto_sync);
        playnite::fullscreen_entry(apps, settings.fullscreen_entry) | synced
    })?;
    if changed {
        tracing::info!(games = games.len(), "Playnite library synced");
    }
    Ok(Outcome {
        changed,
        games: games.len(),
    })
}
struct Watch {
    playnite: Option<u32>,
    settings: String,
    synced: Option<Instant>,
}
static WATCH: Mutex<Watch> = Mutex::new(Watch {
    playnite: None,
    settings: String::new(),
    synced: None,
});
static RUNNING: AtomicBool = AtomicBool::new(false);
/// The auto-sync check, every 30 seconds: keep the fullscreen entry as set,
/// and sync when Playnite starts, the settings change, or every ten minutes.
pub fn watch(h: &Shared) {
    let settings = Settings::from_config(&h.config.read().unwrap());
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let wanted = settings.enabled && settings.fullscreen_entry;
    if let Err(error) =
        crate::steam::update_apps(h, |apps| playnite::fullscreen_entry(apps, wanted))
    {
        tracing::warn!(%error, "the Playnite fullscreen app could not be updated");
    }
    if settings.enabled
        && settings.auto_sync
        && let Some((pid, _)) = butterpollo_windows::playnite::running()
    {
        let fingerprint = format!("{settings:?}");
        let due = {
            let watch = WATCH.lock().unwrap();
            watch.playnite != Some(pid)
                || watch.settings != fingerprint
                || watch
                    .synced
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(600))
        };
        if due {
            if let Err(error) = sync(h) {
                tracing::debug!(error = %format!("{error:#}"), "Playnite auto-sync skipped");
            }
            *WATCH.lock().unwrap() = Watch {
                playnite: Some(pid),
                settings: fingerprint,
                synced: Some(Instant::now()),
            };
        }
    }
    RUNNING.store(false, Ordering::Release);
}

/// `/api/playnite/status`.
pub fn status(h: &Shared) -> Value {
    let settings = Settings::from_config(&h.config.read().unwrap());
    let target = installed_plugin();
    let installed_version = target.as_deref().and_then(version);
    let packaged_version = version(&packaged_plugin(h));
    let installed = target
        .as_ref()
        .is_some_and(|t| PLUGIN_FILES.iter().all(|f| t.join(f).is_file()));
    let catalog = CATALOG.lock().unwrap();
    json!({
        "status": true,
        "enabled": settings.enabled,
        "active": butterpollo_windows::playnite::running().is_some(),
        "available": butterpollo_windows::playnite::install_dir().is_some(),
        "installed": installed,
        "extensions_dir": target.as_ref().and_then(|t| t.parent()),
        "installed_version": installed_version,
        "packaged_version": packaged_version,
        "update_available": match (&installed_version, &packaged_version) {
            (Some(installed), Some(packaged)) => playnite::newer(packaged, installed),
            _ => false,
        },
        "auto_sync": settings.auto_sync,
        "game_count": catalog.games.len(),
        "synced_seconds_ago": catalog.at.map(|at| at.elapsed().as_secs()),
    })
}
fn catalog_ready() -> Result<()> {
    let stale = CATALOG
        .lock()
        .unwrap()
        .at
        .is_none_or(|at| at.elapsed() > Duration::from_secs(60));
    if stale && butterpollo_windows::playnite::running().is_some() {
        snapshot()?;
    }
    Ok(())
}
/// `/api/playnite/games`.
pub fn games() -> Result<Value> {
    catalog_ready()?;
    let catalog = CATALOG.lock().unwrap();
    Ok(Value::Array(
        catalog
            .games
            .iter()
            .map(|g| {
                json!({"id": g.id, "name": g.name, "categories": g.categories, "installed": g.installed,
                       "pluginId": g.plugin_id, "pluginName": g.plugin_name,
                       "lastPlayed": g.last_played.map(playnite::format_time)})
            })
            .collect(),
    ))
}
/// `/api/playnite/categories` (and the plugins seen, for the console).
pub fn categories() -> Result<Value> {
    catalog_ready()?;
    let catalog = CATALOG.lock().unwrap();
    let (names, plugins) = playnite::names(&catalog.games);
    let mut categories: BTreeMap<String, String> = catalog
        .categories
        .iter()
        .map(|(id, name)| (name.clone(), id.clone()))
        .collect();
    for name in names {
        categories.entry(name).or_default();
    }
    let plugins: BTreeSet<(String, String)> =
        catalog.plugins.iter().cloned().chain(plugins).collect();
    Ok(json!({
        "status": true,
        "categories": categories.into_iter().map(|(name, id)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
        "plugins": plugins.into_iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
    }))
}
pub fn uninstall_plugin() -> Result<()> {
    if let Some(target) = installed_plugin()
        && target.is_dir()
    {
        std::fs::remove_dir_all(&target)?;
    }
    Ok(())
}

/// How a game started through Playnite is going.
#[derive(Clone, Debug, PartialEq)]
enum Phase {
    Starting,
    Running,
    Exited,
    /// Started without the plugin: the stream stays until it is ended.
    Untracked,
}
struct LaunchState {
    phase: Phase,
    install_dir: String,
}
/// A Playnite game started for a stream.
pub struct Launch {
    state: Arc<Mutex<LaunchState>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    baseline: Vec<butterpollo_core::steam::Process>,
}
impl Launch {
    /// Start Playnite if needed, then ask the plugin to start game `id` with
    /// the stream's environment.
    pub fn start(h: &Shared, id: &str, environment: &BTreeMap<String, String>) -> Result<Self> {
        let program = butterpollo_windows::playnite::install_dir()
            .and_then(|dir| butterpollo_windows::playnite::executable(&dir))
            .context("Playnite launch failed: no Desktop or Fullscreen executable found; open Playnite once or repair its installation")?;
        tracing::info!(id, executable = %program.display(), running = butterpollo_windows::playnite::running().is_some(), "Playnite launch prepared");
        update_plugin(h);
        let baseline = butterpollo_windows::process::processes().unwrap_or_default();
        let state = Arc::new(Mutex::new(LaunchState {
            phase: Phase::Starting,
            install_dir: String::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let (state, stop) = (state.clone(), stop.clone());
            let (id, environment) = (id.to_owned(), environment.clone());
            std::thread::Builder::new()
                .name("playnite-launch".into())
                .spawn(move || run(&program, &id, &environment, &state, &stop))
                .context("starting the Playnite launch worker")?
        };
        Ok(Self {
            state,
            stop,
            worker: Some(worker),
            baseline,
        })
    }
    /// Whether Playnite reported the game stopped.
    pub fn finished(&self) -> bool {
        self.state.lock().unwrap().phase == Phase::Exited
    }
    /// The game's folder, once Playnite has reported it.
    pub fn folder(&self) -> impl Fn() -> Option<String> + Send + 'static {
        let state = self.state.clone();
        move || Some(state.lock().unwrap().install_dir.clone()).filter(|f| !f.is_empty())
    }
    /// End the game: close the windows of the processes started in its
    /// folder since the launch, then end them.
    pub fn stop(mut self, timeout: Duration) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let state = self.state.lock().unwrap();
        if state.install_dir.trim().is_empty() || state.phase == Phase::Exited {
            return;
        }
        let mut tracker = butterpollo_core::steam::Tracker::new(
            &self.baseline,
            &state.install_dir,
            Duration::ZERO,
        );
        if let Ok(now) = butterpollo_windows::process::processes() {
            tracker.update(&now, butterpollo_windows::process::image_path);
            butterpollo_windows::process::stop_processes(&tracker.tracked, timeout);
        }
    }
}
fn run(
    program: &Path,
    id: &str,
    environment: &BTreeMap<String, String>,
    state: &Mutex<LaunchState>,
    stop: &AtomicBool,
) {
    let set = |phase: Phase| state.lock().unwrap().phase = phase;
    if butterpollo_windows::playnite::running().is_none()
        && let Err(error) = butterpollo_windows::playnite::launch(program, &[], environment)
    {
        tracing::error!(id, error = %format!("{error:#}"), "Playnite startup failed");
        set(Phase::Exited);
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    let hello = json!({"type":"hello","role":"launcher","pid":std::process::id(),"mode":"standard","gameId":id});
    let pipe = loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        match Pipe::connect(&hello) {
            Ok(pipe) => break pipe,
            Err(error) if Instant::now() >= deadline => {
                tracing::warn!(error = %format!("{error:#}"), "the Playnite plugin is not available; starting the game without tracking it");
                match butterpollo_windows::playnite::launch(program, &["--start", id], environment)
                {
                    Ok(()) => set(Phase::Untracked),
                    Err(error) => {
                        tracing::error!(id, error = %format!("{error:#}"), "Playnite fallback launch failed");
                        set(Phase::Exited);
                    }
                }
                return;
            }
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    };
    if let Err(error) =
        pipe.send(&json!({"type":"command","command":"launch","id":id,"env":environment}))
    {
        tracing::warn!(%error, "the Playnite launch request failed");
        set(Phase::Untracked);
        return;
    }
    tracing::info!(id, "asked Playnite to start the game");
    // The connection stays open for the stream: Playnite keeps the
    // stream's environment for the game until it closes.
    while !stop.load(Ordering::Acquire) {
        match pipe.lines.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                let Message::Status {
                    name,
                    id: game,
                    install_dir,
                    ..
                } = playnite::parse(&line)
                else {
                    continue;
                };
                let ours = game.is_empty() || game.eq_ignore_ascii_case(id);
                match name.as_str() {
                    "gameStarted" if ours => {
                        let mut state = state.lock().unwrap();
                        state.phase = Phase::Running;
                        if !install_dir.is_empty() {
                            state.install_dir = install_dir;
                        }
                        tracing::info!(id, folder = %state.install_dir, "Playnite started the game");
                    }
                    "gameStopped" if ours => {
                        tracing::info!(id, "Playnite reports the game stopped");
                        set(Phase::Exited);
                        return;
                    }
                    "playniteExiting" => {
                        tracing::info!("Playnite is closing");
                        set(Phase::Exited);
                        return;
                    }
                    _ => {}
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let mut state = state.lock().unwrap();
                state.phase = if state.phase == Phase::Running {
                    Phase::Exited
                } else {
                    Phase::Untracked
                };
                tracing::info!("the Playnite plugin closed the connection");
                return;
            }
        }
    }
}
/// The command that opens Playnite's fullscreen mode.
pub fn fullscreen_command() -> Option<String> {
    let dir = butterpollo_windows::playnite::install_dir()?;
    let program = butterpollo_windows::playnite::executable(&dir)?;
    Some(format!("\"{}\" --startfullscreen", program.display()))
}
/// Close Playnite's fullscreen mode after its stream.
pub fn close_fullscreen(timeout: Duration) {
    let Ok(processes) = butterpollo_windows::process::processes() else {
        return;
    };
    let fullscreen: BTreeMap<u32, u64> = processes
        .iter()
        .filter(|p| p.name.eq_ignore_ascii_case("Playnite.FullscreenApp.exe"))
        .map(|p| (p.pid, p.started))
        .collect();
    butterpollo_windows::process::stop_processes(&fullscreen, timeout);
}
