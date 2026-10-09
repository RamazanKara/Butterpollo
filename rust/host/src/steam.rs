//! The Steam library sync, as in Vibepollo: apps for the installed or
//! recently played Steam games, with PNG covers, kept in step by a check
//! every 30 seconds while `steam_auto_sync` is on.
use crate::state::Shared;
use anyhow::{Result, bail};
use butterpollo_core::{
    state::App,
    steam::{self, Game, Settings},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

pub struct Outcome {
    pub changed: bool,
    pub games: usize,
    pub importable: usize,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
/// The installed games, or why there are none to read.
pub fn catalog(settings: &Settings) -> Result<(Vec<PathBuf>, Vec<Game>)> {
    if !settings.enabled {
        bail!("Steam integration is disabled");
    }
    let roots = butterpollo_windows::steam::roots();
    if roots.is_empty() {
        bail!("Steam installation was not found");
    }
    let games = steam::discover(&roots);
    Ok((roots, games))
}
/// Sync the managed Steam apps now.
pub fn sync(h: &Shared) -> Result<Outcome> {
    let settings = Settings::from_config(&h.config.read().unwrap());
    let (_, games) = catalog(&settings)?;
    sync_catalog(h, &settings, &games)
}
fn sync_catalog(h: &Shared, settings: &Settings, games: &[Game]) -> Result<Outcome> {
    let selected = steam::select(games, settings, now());
    let covers: HashMap<u32, PathBuf> = selected
        .iter()
        .filter_map(|game| cover(h, game).map(|path| (game.appid, path)))
        .collect();
    let changed = update_apps(h, |apps| {
        steam::reconcile(apps, &selected, settings, &covers)
    })?;
    if changed {
        tracing::info!(selected = selected.len(), "Steam library synced");
    }
    Ok(Outcome {
        changed,
        games: games.len(),
        importable: games
            .iter()
            .filter(|g| steam::importable(g, settings.include_tools))
            .count(),
    })
}
/// Change the app library as JSON and save it when `change` reports a change.
pub fn update_apps(h: &Shared, change: impl FnOnce(&mut Vec<Value>) -> bool) -> Result<bool> {
    let mut apps = h.apps.write().unwrap();
    let mut values = match serde_json::to_value(&*apps)? {
        Value::Array(values) => values,
        _ => vec![],
    };
    if !change(&mut values) {
        return Ok(false);
    }
    let mut next: Vec<App> = serde_json::from_value(Value::Array(values))?;
    h.assign_apps(&mut next)?;
    let mut document = h.app_document.read().unwrap().clone();
    document["apps"] = serde_json::to_value(&next)?;
    butterpollo_core::state::write_json(&h.apps_path, &document)?;
    *apps = next;
    *h.app_document.write().unwrap() = document;
    Ok(true)
}
/// The game's cover as a PNG in the profile's covers folder. A missing or
/// small local portrait is replaced by Steam's 600x900 store image.
fn cover(h: &Shared, game: &Game) -> Option<PathBuf> {
    let folder = h.directory.join("covers");
    let png = folder.join(format!("steam_{}.png", game.appid));
    let mut source = game.artwork().map(Path::to_owned);
    let small = source.as_deref().is_none_or(|path| {
        butterpollo_windows::image::dimensions(path).is_ok_and(|(w, h)| w < 600 || h < 900)
    });
    if small && let Some(download) = store_portrait(&folder, game.appid) {
        source = Some(download);
    }
    let Some(source) = source else {
        return png.is_file().then_some(png);
    };
    let meta = folder.join(format!("steam_{}.png.meta", game.appid));
    let stamp = std::fs::metadata(&source).ok().map(|m| {
        let modified = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        format!(
            "version=1\npath={}\nsize={}\nmtime={modified}\n",
            source.display(),
            m.len()
        )
    })?;
    if png.is_file() && std::fs::read_to_string(&meta).is_ok_and(|saved| saved == stamp) {
        return Some(png);
    }
    match butterpollo_windows::image::to_png(&source, &png) {
        Ok(_) => {
            let _ = butterpollo_core::state::atomic_write(&meta, stamp.as_bytes());
            Some(png)
        }
        Err(error) => {
            tracing::debug!(error = %format!("{error:#}"), appid = game.appid, "Steam cover conversion failed");
            png.is_file().then_some(png)
        }
    }
}
/// Steam's portrait store image, downloaded once into the covers folder. A
/// failed download is not retried for ten minutes.
fn store_portrait(folder: &Path, appid: u32) -> Option<PathBuf> {
    let file = folder.join(format!("steam_{appid}_600x900_2x.jpg"));
    let valid = |path: &Path| {
        butterpollo_windows::image::dimensions(path).is_ok_and(|(w, h)| w >= 600 && h >= 900)
    };
    if file.is_file() && valid(&file) {
        return Some(file);
    }
    let failed = folder.join(format!("steam_{appid}_600x900_2x.jpg.failed"));
    if std::fs::metadata(&failed)
        .and_then(|m| m.modified())
        .is_ok_and(|at| at.elapsed().is_ok_and(|age| age < Duration::from_secs(600)))
    {
        return None;
    }
    let url = format!(
        "https://shared.fastly.steamstatic.com/store_item_assets/steam/apps/{appid}/library_600x900_2x.jpg"
    );
    let download = || -> Result<Vec<u8>> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .user_agent("Rubylight-Steam-Artwork/1.0")
            .build()?;
        tokio::runtime::Handle::current().block_on(async {
            let response = client.get(&url).send().await?.error_for_status()?;
            if response.content_length().is_some_and(|n| n > 16 << 20) {
                bail!("Steam artwork is too large");
            }
            let bytes = response.bytes().await?;
            if bytes.len() > 16 << 20 {
                bail!("Steam artwork is too large");
            }
            Ok(bytes.to_vec())
        })
    };
    let result = download().and_then(|bytes| {
        std::fs::create_dir_all(folder)?;
        butterpollo_core::state::atomic_write(&file, &bytes)?;
        if !valid(&file) {
            let _ = std::fs::remove_file(&file);
            bail!("Steam artwork is smaller than 600x900");
        }
        Ok(())
    });
    match result {
        Ok(()) => {
            let _ = std::fs::remove_file(&failed);
            Some(file)
        }
        Err(error) => {
            tracing::debug!(error = %format!("{error:#}"), appid, "Steam store artwork unavailable");
            let _ = std::fs::create_dir_all(folder);
            let _ = std::fs::write(&failed, b"");
            None
        }
    }
}
/// What the files a sync reads look like now: sizes and times of the
/// library lists, manifests, play history, app cache and artwork folders.
fn fingerprint(settings: &Settings, roots: &[PathBuf]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{settings:?}").hash(&mut hasher);
    let mut stamp = |path: &Path| {
        path.hash(&mut hasher);
        if let Ok(metadata) = std::fs::metadata(path) {
            metadata.len().hash(&mut hasher);
            metadata.modified().ok().hash(&mut hasher);
        }
    };
    let mut libraries = vec![];
    for root in roots {
        let Some(apps) = steam::steamapps(root) else {
            continue;
        };
        stamp(&apps.join("libraryfolders.vdf"));
        stamp(&root.join("appcache").join("appinfo.vdf"));
        stamp(&root.join("appcache").join("librarycache"));
        if let Ok(users) = std::fs::read_dir(root.join("userdata")) {
            for user in users.flatten() {
                stamp(&user.path().join("config").join("localconfig.vdf"));
            }
        }
        libraries.push(apps.clone());
        if let Ok(text) = std::fs::read_to_string(apps.join("libraryfolders.vdf")) {
            let folders = steam::Vdf::parse(&text);
            let folders = folders.get("libraryfolders").unwrap_or(&folders);
            for (_, folder) in folders.entries() {
                if let Some(path) = folder
                    .get("path")
                    .or(Some(folder))
                    .and_then(steam::Vdf::text)
                    && let Some(apps) = steam::steamapps(Path::new(path))
                {
                    libraries.push(apps);
                }
            }
        }
    }
    for library in libraries {
        if let Ok(entries) = std::fs::read_dir(&library) {
            let mut manifests: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("acf")))
                .collect();
            manifests.sort();
            for manifest in manifests {
                stamp(&manifest);
            }
        }
    }
    hasher.finish()
}
struct Watch {
    fingerprint: u64,
    synced: Option<Instant>,
}
static WATCH: Mutex<Watch> = Mutex::new(Watch {
    fingerprint: 0,
    synced: None,
});
static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The auto-sync check: sync when the Steam files or settings changed, and
/// at least every five minutes to retry artwork.
pub fn watch(h: &Shared) {
    let settings = Settings::from_config(&h.config.read().unwrap());
    if !settings.enabled || !settings.auto_sync {
        return;
    }
    if RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let roots = butterpollo_windows::steam::roots();
    let current = fingerprint(&settings, &roots);
    let due = {
        let watch = WATCH.lock().unwrap();
        watch.fingerprint != current
            || watch
                .synced
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(300))
    };
    if due {
        match sync(h) {
            Ok(_) => {
                let mut watch = WATCH.lock().unwrap();
                watch.fingerprint = current;
                watch.synced = Some(Instant::now());
            }
            Err(error) => {
                tracing::debug!(error = %format!("{error:#}"), "Steam auto-sync skipped");
                WATCH.lock().unwrap().synced = Some(Instant::now());
            }
        }
    }
    RUNNING.store(false, std::sync::atomic::Ordering::Release);
}
/// `/api/steam/status`.
pub fn status(h: &Shared) -> Value {
    let settings = Settings::from_config(&h.config.read().unwrap());
    let roots = butterpollo_windows::steam::roots();
    let games = if roots.is_empty() {
        vec![]
    } else {
        steam::discover(&roots)
    };
    let installed: Vec<&Game> = games.iter().filter(|g| g.installed).collect();
    let importable = installed
        .iter()
        .filter(|g| steam::importable(g, settings.include_tools))
        .count();
    let tools = installed
        .iter()
        .filter(|g| !steam::importable(g, false))
        .count();
    let excluded = installed
        .iter()
        .filter(|g| steam::is_excluded(g, &settings.exclusions))
        .count();
    json!({
        "status": true,
        "provider": "steam",
        "enabled": settings.enabled,
        "forced": false,
        "available": !roots.is_empty(),
        "roots": roots,
        "game_count": installed.len(),
        "importable_game_count": importable,
        "tool_game_count": tools,
        "excluded_game_count": excluded,
        "selected_game_count": steam::select(&games, &settings, now()).len(),
        "exclude_games": settings.exclusions.iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
        "auto_sync": settings.auto_sync,
        "sync_all_installed": settings.sync_all_installed,
        "recent_games": settings.recent_games,
        "recent_max_age_days": settings.recent_max_age_days,
        "autosync_remove_uninstalled": settings.remove_uninstalled,
        "include_tools": settings.include_tools,
        "playnite_available": false,
    })
}
/// `/api/steam/games`, optionally one app with its cover prepared.
pub fn games(h: &Shared, appid: Option<u32>) -> Result<Value> {
    let settings = Settings::from_config(&h.config.read().unwrap());
    let (_, games) = catalog(&settings)?;
    let selected: Vec<u32> = steam::select(&games, &settings, now())
        .iter()
        .map(|g| g.appid)
        .collect();
    let list: Vec<Value> = games
        .iter()
        .filter(|g| appid.is_none_or(|id| id == g.appid))
        .map(|game| {
            let cover = appid.and_then(|_| cover(h, game));
            let path = |p: &Option<PathBuf>| p.as_ref().map(|p| p.to_string_lossy().into_owned());
            let importable = steam::importable(game, settings.include_tools);
            let excluded = steam::is_excluded(game, &settings.exclusions);
            json!({
                "appid": game.appid,
                "steam_id": game.appid.to_string(),
                "stable_id": format!("steam:{}", game.appid),
                "name": game.name,
                "install_dir": game.install_dir,
                "library_path": game.library_path,
                "icon_path": path(&game.icon_path),
                "header_path": path(&game.header_path),
                "portrait_path": path(&game.portrait_path),
                "artwork_path": game.artwork(),
                "artwork_client_path": cover,
                "app_type": game.app_type,
                "installed": game.installed,
                "last_played": game.last_played,
                "playtime_minutes": game.playtime_minutes,
                "importable": importable,
                "excluded": excluded,
                "selected": selected.contains(&game.appid),
                "launch_uri": format!("steam://rungameid/{}", game.appid),
            })
        })
        .collect();
    Ok(json!({"status": true, "enabled": settings.enabled, "games": list}))
}
/// `/api/steam/launch`: start an installed game on this PC through Steam.
pub fn launch(h: &Shared, appid: u32) -> Result<Value> {
    let _transition = h.launch_transition.lock().unwrap();
    if crate::updater::installing(h) {
        bail!("Rubylight is installing an update");
    }
    let settings = Settings::from_config(&h.config.read().unwrap());
    let (_, games) = catalog(&settings)?;
    let Some(game) = games.iter().find(|g| g.appid == appid && g.installed) else {
        bail!("Steam game {appid} is not installed");
    };
    if !steam::importable(game, settings.include_tools)
        || steam::is_excluded(game, &settings.exclusions)
    {
        bail!("Steam app {appid} is excluded");
    }
    let environment = butterpollo_windows::process::user_environment()?;
    butterpollo_windows::process::Process::shell_detached(
        &steam::launch_command(appid),
        None,
        false,
        &environment,
    )?;
    Ok(json!({"status": true, "launch_uri": format!("steam://rungameid/{appid}")}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::Fixture;
    use butterpollo_core::config::Config;

    #[test]
    fn steam_sync_converts_local_covers_persists_apps_and_is_idempotent() {
        let f = Fixture::new();
        let cover = f.host.directory.join("portrait.bmp");
        // An uncompressed 600x900 BGR bitmap avoids both codec/GPU dependencies and store downloads.
        let mut bmp = vec![0u8; 54 + 600 * 900 * 3];
        let size = bmp.len() as u32;
        bmp[..2].copy_from_slice(b"BM");
        bmp[2..6].copy_from_slice(&size.to_le_bytes());
        bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&600u32.to_le_bytes());
        bmp[22..26].copy_from_slice(&900u32.to_le_bytes());
        bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
        bmp[28..30].copy_from_slice(&24u16.to_le_bytes());
        std::fs::write(&cover, bmp).unwrap();
        let games = [Game {
            appid: 570,
            name: "Fixture game".into(),
            app_type: "Game".into(),
            installed: true,
            portrait_path: Some(cover),
            ..Default::default()
        }];
        let mut settings = Settings::from_config(
            &Config::parse("steam_enabled=true\nsteam_sync_all_installed=true").unwrap(),
        );
        let original = f.host.apps.read().unwrap().len();
        let first = sync_catalog(&f.host, &settings, &games).unwrap();
        assert!(first.changed);
        assert_eq!((first.games, first.importable), (1, 1));
        let saved = butterpollo_core::state::load_json(&f.host.apps_path, json!({})).unwrap();
        assert_eq!(saved["apps"].as_array().unwrap().len(), original + 1);
        let app = saved["apps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|app| app["steam-id"] == "570")
            .unwrap();
        let png = Path::new(app["image-path"].as_str().unwrap());
        assert_eq!(&std::fs::read(png).unwrap()[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            butterpollo_windows::image::dimensions(png).unwrap(),
            (600, 900)
        );
        assert!(!sync_catalog(&f.host, &settings, &games).unwrap().changed);
        assert_eq!(
            butterpollo_core::state::load_json(&f.host.apps_path, json!({})).unwrap(),
            saved
        );
        settings.exclusions.push(("570".into(), String::new()));
        assert!(sync_catalog(&f.host, &settings, &games).unwrap().changed);
        assert_eq!(f.host.apps.read().unwrap().len(), original);
    }

    #[test]
    fn steam_watch_fingerprint_detects_manifest_and_settings_changes_without_waiting() {
        let f = Fixture::new();
        let library = f.host.directory.join("steamapps");
        std::fs::create_dir(&library).unwrap();
        let manifest = library.join("appmanifest_570.acf");
        std::fs::write(&manifest, "first").unwrap();
        let roots = [f.host.directory.clone()];
        let mut settings = Settings::from_config(&Config::default());
        let first = fingerprint(&settings, &roots);
        assert_eq!(fingerprint(&settings, &roots), first);
        std::fs::write(&manifest, "changed size").unwrap();
        let changed = fingerprint(&settings, &roots);
        assert_ne!(changed, first);
        settings.sync_all_installed = true;
        assert_ne!(fingerprint(&settings, &roots), changed);
    }
}
