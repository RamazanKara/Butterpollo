//! Steam library discovery and the apps it maintains, as in Vibepollo 2.0.
//!
//! Only local files are read: `libraryfolders.vdf`, `appmanifest_*.acf`,
//! `userdata/*/config/localconfig.vdf`, `appcache/appinfo.vdf` and the
//! library cache artwork. Apps the sync owns carry `steam-managed: "auto"`
//! and a UUID derived from the Steam app ID.
use crate::config::Config;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

/// A KeyValues document (Valve's text or binary VDF).
#[derive(Clone, Debug, PartialEq)]
pub enum Vdf {
    Text(String),
    Object(Vec<(String, Vdf)>),
}
impl Vdf {
    /// Parse text VDF leniently, as Vibepollo does: a stray brace or a
    /// missing value never stops the rest of the file.
    pub fn parse(text: &str) -> Self {
        let mut lexer = Lexer {
            text: text.as_bytes(),
            at: 0,
        };
        Self::Object(lexer.body(false))
    }
    /// A child by key, ignoring case.
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        self.entries()
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value)
    }
    pub fn at(&self, keys: &[&str]) -> Option<&Vdf> {
        keys.iter().try_fold(self, |node, key| node.get(key))
    }
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Object(_) => None,
        }
    }
    pub fn number(&self) -> Option<u64> {
        self.text()?.trim().parse().ok()
    }
    pub fn entries(&self) -> &[(String, Vdf)] {
        match self {
            Self::Object(entries) => entries,
            Self::Text(_) => &[],
        }
    }
}
struct Lexer<'a> {
    text: &'a [u8],
    at: usize,
}
impl Lexer<'_> {
    fn space(&mut self) {
        while let Some(&c) = self.text.get(self.at) {
            if c.is_ascii_whitespace() {
                self.at += 1;
            } else if self.text[self.at..].starts_with(b"//") {
                while self.text.get(self.at).is_some_and(|&c| c != b'\n') {
                    self.at += 1;
                }
            } else {
                break;
            }
        }
    }
    fn token(&mut self) -> Option<String> {
        self.space();
        match self.text.get(self.at)? {
            b'{' | b'}' => None,
            b'"' => {
                self.at += 1;
                let mut out = vec![];
                while let Some(&c) = self.text.get(self.at) {
                    self.at += 1;
                    match c {
                        b'"' => break,
                        b'\\' if self.at < self.text.len() => {
                            let escaped = self.text[self.at];
                            self.at += 1;
                            out.push(match escaped {
                                b'n' => b'\n',
                                b't' => b'\t',
                                b'r' => b'\r',
                                other => other,
                            });
                        }
                        _ => out.push(c),
                    }
                }
                Some(String::from_utf8_lossy(&out).into_owned())
            }
            _ => {
                let start = self.at;
                while self
                    .text
                    .get(self.at)
                    .is_some_and(|c| !c.is_ascii_whitespace() && !matches!(c, b'{' | b'}'))
                {
                    self.at += 1;
                }
                Some(String::from_utf8_lossy(&self.text[start..self.at]).into_owned())
            }
        }
    }
    fn body(&mut self, nested: bool) -> Vec<(String, Vdf)> {
        let mut entries = vec![];
        loop {
            self.space();
            let Some(&c) = self.text.get(self.at) else {
                return entries;
            };
            if nested && c == b'}' {
                self.at += 1;
                return entries;
            }
            let Some(key) = self.token() else {
                // A stray brace cannot form a pair; skip it.
                self.at += 1;
                continue;
            };
            self.space();
            if self.text.get(self.at) == Some(&b'{') {
                self.at += 1;
                entries.push((key, Vdf::Object(self.body(true))));
            } else if let Some(value) = self.token() {
                entries.push((key, Vdf::Text(value)));
            } else {
                entries.push((key, Vdf::Object(vec![])));
            }
        }
    }
}

/// Read the `common` names and types from Steam's binary app cache
/// (`appcache/appinfo.vdf`, format 28 or 29) for the apps in `wanted`.
pub fn appinfo(data: &[u8], wanted: &BTreeSet<u32>) -> BTreeMap<u32, (String, String)> {
    let mut found = BTreeMap::new();
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
    };
    let (keys, end) = match u32_at(0) {
        Some(0x0756_4429) => {
            // Keys are indices into a string table at the end of the file.
            let Some(table) = data
                .get(8..16)
                .and_then(|b| usize::try_from(u64::from_le_bytes(b.try_into().ok()?)).ok())
            else {
                return found;
            };
            let Some(count) = u32_at(table) else {
                return found;
            };
            let mut keys = Vec::with_capacity(count.min(1 << 20) as usize);
            let mut at = table + 4;
            for _ in 0..count {
                let Some(length) = data
                    .get(at..)
                    .and_then(|rest| rest.iter().position(|b| *b == 0))
                else {
                    return found;
                };
                keys.push(String::from_utf8_lossy(&data[at..at + length]).into_owned());
                at += length + 1;
            }
            (Some(keys), table)
        }
        Some(0x0756_4428) => (None, data.len()),
        _ => return found,
    };
    let mut at = if keys.is_some() { 16 } else { 8 };
    while let (Some(app), Some(size)) = (u32_at(at), u32_at(at + 4)) {
        if app == 0 {
            break;
        }
        let entry_end = at + 8 + size as usize;
        if size < 60 || entry_end > end {
            break;
        }
        if wanted.contains(&app) {
            let mut cursor = at + 68;
            if let Some(root) = binary_object(data, &mut cursor, entry_end, keys.as_deref()) {
                let info = root.get("appinfo").unwrap_or(&root);
                let common = |key: &str| {
                    info.at(&["common", key])
                        .and_then(Vdf::text)
                        .unwrap_or("")
                        .to_owned()
                };
                found.insert(app, (common("name"), common("type")));
            }
        }
        at = entry_end;
    }
    found
}
fn binary_object(data: &[u8], at: &mut usize, end: usize, keys: Option<&[String]>) -> Option<Vdf> {
    let mut entries = vec![];
    let cstring = |at: &mut usize| -> Option<String> {
        let length = data.get(*at..end)?.iter().position(|b| *b == 0)?;
        let text = String::from_utf8_lossy(&data[*at..*at + length]).into_owned();
        *at += length + 1;
        Some(text)
    };
    let fixed = |at: &mut usize, size: usize| -> Option<u64> {
        let bytes = data.get(*at..(*at + size).min(end))?;
        if bytes.len() != size {
            return None;
        }
        *at += size;
        Some(
            bytes
                .iter()
                .rev()
                .fold(0u64, |value, byte| value << 8 | u64::from(*byte)),
        )
    };
    while *at < end {
        let kind = data[*at];
        *at += 1;
        if matches!(kind, 0x08 | 0x0b) {
            return Some(Vdf::Object(entries));
        }
        let key = match keys {
            Some(keys) => keys.get(fixed(at, 4)? as usize)?.clone(),
            None => cstring(at)?,
        };
        let value = match kind {
            0x00 => binary_object(data, at, end, keys)?,
            0x01 => Vdf::Text(cstring(at)?),
            0x02..=0x04 | 0x06 => Vdf::Text(fixed(at, 4)?.to_string()),
            0x07 | 0x0a => Vdf::Text(fixed(at, 8)?.to_string()),
            0x05 => {
                let mut units = vec![];
                loop {
                    let unit = fixed(at, 2)? as u16;
                    if unit == 0 {
                        break;
                    }
                    units.push(unit);
                }
                Vdf::Text(String::from_utf16_lossy(&units))
            }
            _ => return None,
        };
        entries.push((key, value));
    }
    None
}

/// An installed (or recently played) Steam app.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Game {
    pub appid: u32,
    pub name: String,
    pub app_type: String,
    pub install_dir: PathBuf,
    pub library_path: PathBuf,
    pub installed: bool,
    /// Unix seconds; 0 when never played.
    pub last_played: u64,
    pub playtime_minutes: u64,
    pub portrait_path: Option<PathBuf>,
    pub header_path: Option<PathBuf>,
    pub icon_path: Option<PathBuf>,
}
impl Game {
    /// The best local cover: portrait, else header, else icon.
    pub fn artwork(&self) -> Option<&Path> {
        self.portrait_path
            .as_deref()
            .or(self.header_path.as_deref())
            .or(self.icon_path.as_deref())
    }
}

/// The `steamapps` folder of a Steam or library root.
pub fn steamapps(root: &Path) -> Option<PathBuf> {
    let named = root
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("steamapps"));
    if (named && root.is_dir()) || root.join("libraryfolders.vdf").is_file() {
        return Some(root.to_owned());
    }
    let nested = root.join("steamapps");
    nested.is_dir().then_some(nested)
}
/// Every installed app in the libraries of `roots` (Steam installations),
/// with play history, names and types from the app cache, and artwork.
pub fn discover(roots: &[PathBuf]) -> Vec<Game> {
    let mut libraries: Vec<PathBuf> = vec![];
    let add = |path: PathBuf, libraries: &mut Vec<PathBuf>| {
        if !libraries.iter().any(|known| same_path(known, &path)) {
            libraries.push(path);
        }
    };
    for root in roots {
        let Some(apps) = steamapps(root) else {
            continue;
        };
        add(apps.clone(), &mut libraries);
        let folders = std::fs::read_to_string(apps.join("libraryfolders.vdf"))
            .map(|text| Vdf::parse(&text))
            .unwrap_or(Vdf::Object(vec![]));
        let folders = folders.get("libraryfolders").unwrap_or(&folders);
        for (_, folder) in folders.entries() {
            // Old files give the path as the value itself.
            let path = folder.get("path").or(Some(folder)).and_then(Vdf::text);
            if let Some(path) = path.filter(|p| !p.is_empty())
                && let Some(library) = steamapps(Path::new(path))
            {
                add(library, &mut libraries);
            }
        }
    }
    let mut games: BTreeMap<u32, Game> = BTreeMap::new();
    for steamapps in &libraries {
        let Ok(entries) = std::fs::read_dir(steamapps) else {
            continue;
        };
        let mut manifests: Vec<_> = entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|name| {
                        let name = name.to_ascii_lowercase();
                        name.starts_with("appmanifest_") && name.ends_with(".acf")
                    })
            })
            .collect();
        manifests.sort();
        for manifest in manifests {
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            let document = Vdf::parse(&text);
            let Some(state) = document.get("AppState") else {
                continue;
            };
            let Some(appid) = state
                .get("appid")
                .and_then(Vdf::number)
                .and_then(|id| u32::try_from(id).ok())
                .filter(|id| *id > 0)
            else {
                continue;
            };
            let folder = state.get("installdir").and_then(Vdf::text).unwrap_or("");
            let install_dir = steamapps.join("common").join(folder);
            if folder.is_empty() || !install_dir.is_dir() || games.contains_key(&appid) {
                continue;
            }
            games.insert(
                appid,
                Game {
                    appid,
                    name: state.get("name").and_then(Vdf::text).unwrap_or("").into(),
                    app_type: state.get("type").and_then(Vdf::text).unwrap_or("").into(),
                    install_dir,
                    library_path: steamapps.parent().unwrap_or(steamapps).to_owned(),
                    installed: true,
                    ..Default::default()
                },
            );
        }
    }
    let steam_roots: Vec<&PathBuf> = roots
        .iter()
        .filter(|r| r.join("userdata").is_dir())
        .collect();
    for root in &steam_roots {
        apply_history(root, &mut games);
    }
    let wanted: BTreeSet<u32> = games.keys().copied().collect();
    for root in &steam_roots {
        if let Ok(data) = std::fs::read(root.join("appcache").join("appinfo.vdf")) {
            for (appid, (name, kind)) in appinfo(&data, &wanted) {
                let game = games.get_mut(&appid).unwrap();
                if !name.is_empty() {
                    game.name = name;
                }
                if !kind.is_empty() {
                    game.app_type = kind.to_ascii_lowercase();
                }
            }
            break;
        }
    }
    for game in games.values_mut() {
        let (portrait, header, icon) = artwork(game.appid, &game.library_path, roots);
        game.portrait_path = portrait;
        game.header_path = header;
        game.icon_path = icon;
    }
    games.into_values().filter(|g| !g.name.is_empty()).collect()
}
fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .eq_ignore_ascii_case(
            b.to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\'),
        )
}
/// LastPlayed and Playtime from every signed-in user's local config.
fn apply_history(root: &Path, games: &mut BTreeMap<u32, Game>) {
    let Ok(users) = std::fs::read_dir(root.join("userdata")) else {
        return;
    };
    for user in users.flatten() {
        let Ok(text) = std::fs::read_to_string(user.path().join("config").join("localconfig.vdf"))
        else {
            continue;
        };
        let document = Vdf::parse(&text);
        let Some(apps) =
            document.at(&["UserLocalConfigStore", "Software", "Valve", "Steam", "apps"])
        else {
            continue;
        };
        for (id, app) in apps.entries() {
            let Some(game) = id.parse().ok().and_then(|id: u32| games.get_mut(&id)) else {
                continue;
            };
            if let Some(last) = app.get("LastPlayed").and_then(Vdf::number) {
                game.last_played = game.last_played.max(last);
            }
            if let Some(minutes) = app.get("Playtime").and_then(Vdf::number) {
                game.playtime_minutes = game.playtime_minutes.max(minutes);
            }
        }
    }
}
type Artwork = (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>);
/// Portrait, header and icon from Steam's library cache, or the user's
/// custom grid artwork.
pub fn artwork(appid: u32, library: &Path, roots: &[PathBuf]) -> Artwork {
    let mut caches = vec![];
    let mut grids = vec![];
    for root in std::iter::once(library).chain(roots.iter().map(PathBuf::as_path)) {
        caches.push(root.join("appcache").join("librarycache"));
        caches.push(root.join("librarycache"));
        if let Ok(users) = std::fs::read_dir(root.join("userdata")) {
            let mut users: Vec<_> = users.flatten().map(|u| u.path()).collect();
            users.sort();
            grids.extend(users.into_iter().map(|u| u.join("config").join("grid")));
        }
    }
    let id = appid.to_string();
    let flat = |dirs: &[PathBuf], suffixes: &[String]| {
        dirs.iter()
            .flat_map(|dir| suffixes.iter().map(|s| dir.join(format!("{id}{s}"))))
            .find(|path| path.is_file())
    };
    let nested = |names: &[String]| {
        caches
            .iter()
            .flat_map(|dir| names.iter().map(|n| dir.join(&id).join(n)))
            .find(|path| path.is_file())
    };
    // Current clients keep store assets under <appid>/<content hash>/.
    let hashed = |names: &[String]| {
        caches.iter().find_map(|dir| {
            let mut folders: Vec<_> = std::fs::read_dir(dir.join(&id))
                .ok()?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            folders.sort();
            folders
                .iter()
                .flat_map(|folder| names.iter().map(|n| folder.join(n)))
                .find(|path| path.is_file())
        })
    };
    let images = |stem: &str| ["jpg", "png", "webp"].map(|ext| format!("{stem}.{ext}"));
    let grid = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    let portrait = flat(&caches, &images("_library_600x900_2x"))
        .or_else(|| nested(&images("library_600x900_2x")))
        .or_else(|| flat(&caches, &images("_library_600x900")))
        .or_else(|| nested(&images("library_600x900")))
        .or_else(|| hashed(&images("library_capsule")))
        .or_else(|| flat(&grids, &grid(&["p.png", "p.jpg", "_p.png", "_p.jpg"])));
    let header = flat(&caches, &images("_header"))
        .or_else(|| nested(&images("header")))
        .or_else(|| hashed(&images("library_header")))
        .or_else(|| flat(&grids, &grid(&["_hero.png", "_hero.jpg"])));
    let icon = flat(&caches, &images("_icon"))
        .or_else(|| nested(&images("icon")))
        .or_else(|| flat(&grids, &grid(&["_icon.png", "_icon.jpg", ".png"])));
    (portrait, header, icon)
}

/// The Steam settings (`steam_*` keys, Vibepollo's defaults).
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub auto_sync: bool,
    pub sync_all_installed: bool,
    pub recent_games: usize,
    pub recent_max_age_days: u64,
    pub remove_uninstalled: bool,
    pub include_tools: bool,
    pub exclusions: Vec<(String, String)>,
}
impl Settings {
    pub fn from_config(config: &Config) -> Self {
        let count = |key: &str, default: i64| config.integer(key, default).max(0) as u64;
        Self {
            enabled: config.boolean("steam_enabled", false),
            auto_sync: config.boolean("steam_auto_sync", false),
            sync_all_installed: config.boolean("steam_sync_all_installed", false),
            recent_games: count("steam_recent_games", 10) as usize,
            recent_max_age_days: count("steam_recent_max_age_days", 30),
            remove_uninstalled: config.boolean("steam_autosync_remove_uninstalled", true),
            include_tools: config.boolean("steam_include_tools", false),
            exclusions: id_names(config.get("steam_exclude_games", "")),
        }
    }
    /// Whether a sync removes managed apps that are no longer selected.
    pub fn removes_missing(&self) -> bool {
        !self.sync_all_installed || self.remove_uninstalled
    }
    pub fn source(&self) -> &'static str {
        if self.sync_all_installed {
            "installed"
        } else {
            "recent"
        }
    }
}
/// `[{"id":"570","name":"Dota 2"}, "730"]` or `570, 730`: plain entries are IDs.
pub fn id_names(value: &str) -> Vec<(String, String)> {
    let value = value.trim();
    if value.is_empty() {
        return vec![];
    }
    match serde_json::from_str::<Value>(value) {
        Ok(Value::Array(entries)) => entries
            .iter()
            .filter_map(|entry| match entry {
                Value::String(id) => Some((id.trim().to_owned(), String::new())),
                Value::Number(id) => Some((id.to_string(), String::new())),
                Value::Object(fields) => {
                    let text = |key: &str| match fields.get(key) {
                        Some(Value::String(s)) => s.trim().to_owned(),
                        Some(Value::Number(n)) => n.to_string(),
                        _ => String::new(),
                    };
                    Some((text("id"), text("name")))
                }
                _ => None,
            })
            .filter(|(id, name)| !id.is_empty() || !name.is_empty())
            .collect(),
        _ => value
            .split(',')
            .map(|id| (id.trim().to_owned(), String::new()))
            .filter(|(id, _)| !id.is_empty())
            .collect(),
    }
}
/// Not a game: tools, runtimes, redistributables and DLC.
pub fn importable(game: &Game, include_tools: bool) -> bool {
    if include_tools {
        return true;
    }
    if matches!(
        game.app_type.to_ascii_lowercase().as_str(),
        "tool" | "config" | "dlc" | "driver" | "music" | "video"
    ) || [228980, 1070560, 1391110, 1628350, 1824220, 1493710, 4183110].contains(&game.appid)
    {
        return false;
    }
    let name = game.name.to_ascii_lowercase();
    !name.starts_with("proton ")
        && !name.starts_with("steam linux runtime")
        && name != "steamworks common redistributables"
}
fn excluded(id: &str, name: &str, exclusions: &[(String, String)]) -> bool {
    exclusions.iter().any(|(excluded_id, excluded_name)| {
        (!excluded_id.is_empty() && excluded_id == id)
            || (!excluded_name.is_empty() && excluded_name.eq_ignore_ascii_case(name))
    })
}
pub fn is_excluded(game: &Game, exclusions: &[(String, String)]) -> bool {
    excluded(&game.appid.to_string(), &game.name, exclusions)
}
/// The games a sync keeps: every installed one, or the most recently played.
pub fn select<'a>(games: &'a [Game], settings: &Settings, now: u64) -> Vec<&'a Game> {
    let mut selected: Vec<&Game> = games
        .iter()
        .filter(|g| importable(g, settings.include_tools) && !is_excluded(g, &settings.exclusions))
        .filter(|g| g.installed && (settings.sync_all_installed || g.last_played > 0))
        .collect();
    if settings.sync_all_installed {
        return selected;
    }
    if settings.recent_games == 0 {
        return vec![];
    }
    if settings.recent_max_age_days > 0 {
        let age = settings.recent_max_age_days * 86400;
        selected.retain(|g| g.last_played <= now && now - g.last_played <= age);
    }
    selected.sort_by(|a, b| {
        b.last_played
            .cmp(&a.last_played)
            .then(a.appid.cmp(&b.appid))
    });
    selected.truncate(settings.recent_games);
    selected
}
/// Vibepollo's UUID for a Steam app: "STEAM" in the first fields and the
/// app ID in the last 48 bits.
pub fn canonical_uuid(appid: u32) -> String {
    format!("53544541-4d00-5000-8000-{appid:012x}")
}
/// The Steam app ID an app entry names, from `steam-id` or its UUID.
pub fn app_id_of(app: &Value) -> Option<u32> {
    match app.get("steam-id") {
        Some(Value::Number(id)) => {
            if let Some(id) = id.as_u64().and_then(|id| u32::try_from(id).ok()) {
                return Some(id);
            }
        }
        Some(Value::String(raw)) => {
            if let Ok(id) = raw.parse() {
                return Some(id);
            }
            if raw.to_ascii_lowercase().starts_with("steam-") {
                return raw[6..].parse().ok();
            }
        }
        _ => {}
    }
    let uuid = app.get("uuid")?.as_str()?.to_ascii_lowercase();
    if let Some(id) = uuid.strip_prefix("steam-") {
        return id.parse().ok();
    }
    uuid.strip_prefix("53544541-4d00-5000-8000-")
        .filter(|hex| hex.len() == 12)
        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
}
fn id_of(app: &Value) -> String {
    app_id_of(app).map_or_else(
        || {
            app.get("steam-id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        },
        |id| id.to_string(),
    )
}
/// How a Steam game starts: through the Steam client, so Steam's overlay,
/// cloud saves and launch options apply.
pub fn launch_command(appid: u32) -> String {
    format!("cmd /c start \"\" steam://rungameid/{appid}")
}
/// The fallback cover for Steam apps without artwork.
pub const FALLBACK_IMAGE: &str = "./assets/steam.png";
fn generic(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
fn set_or_remove(app: &mut serde_json::Map<String, Value>, key: &str, value: Option<String>) {
    match value.filter(|v| !v.is_empty()) {
        Some(value) => {
            app.insert(key.into(), Value::String(value));
        }
        None => {
            app.remove(key);
        }
    }
}
/// Write a managed app's fields for `game`; `cover` is its PNG, if any.
fn update(app: &mut Value, game: &Game, source: &str, cover: Option<&Path>) {
    if !app.is_object() {
        *app = json!({});
    }
    let fields = app.as_object_mut().unwrap();
    let name = if game.name.is_empty() {
        format!("Steam {}", game.appid)
    } else {
        game.name.clone()
    };
    fields.insert("name".into(), json!(name));
    fields.insert("uuid".into(), json!(canonical_uuid(game.appid)));
    fields.insert("steam-id".into(), json!(game.appid.to_string()));
    fields.insert("steam-managed".into(), json!("auto"));
    fields.insert("steam-source".into(), json!(source));
    set_or_remove(fields, "steam-app-type", Some(game.app_type.clone()));
    fields.insert("cmd".into(), json!(launch_command(game.appid)));
    fields.insert("auto-detach".into(), json!(true));
    fields.insert("wait-all".into(), json!(false));
    set_or_remove(
        fields,
        "steam-install-dir",
        Some(generic(&game.install_dir)),
    );
    set_or_remove(
        fields,
        "steam-library-path",
        Some(generic(&game.library_path)),
    );
    set_or_remove(
        fields,
        "steam-icon-path",
        game.icon_path.as_deref().map(generic),
    );
    set_or_remove(
        fields,
        "steam-header-path",
        game.header_path.as_deref().map(generic),
    );
    set_or_remove(
        fields,
        "steam-boxart-path",
        game.portrait_path.as_deref().map(generic),
    );
    let artwork = game.artwork();
    set_or_remove(fields, "steam-artwork-path", artwork.map(generic));
    set_or_remove(
        fields,
        "steam-artwork-format",
        artwork
            .and_then(|p| p.extension())
            .map(|e| e.to_string_lossy().to_ascii_lowercase()),
    );
    match cover {
        Some(cover) => {
            let cover = generic(cover);
            fields.insert("steam-artwork-client-path".into(), json!(cover));
            fields.insert("steam-artwork-client-compatible".into(), json!(true));
            fields.insert("image-path".into(), json!(cover));
        }
        None => {
            // Keep a cover the user chose; replace only the one this sync set.
            let previous = fields
                .remove("steam-artwork-client-path")
                .and_then(|v| v.as_str().map(str::to_owned));
            fields.remove("steam-artwork-client-compatible");
            let image = fields
                .get("image-path")
                .and_then(Value::as_str)
                .unwrap_or("");
            if image.is_empty() || previous.as_deref() == Some(image) {
                fields.insert("image-path".into(), json!(FALLBACK_IMAGE));
            }
        }
    }
}
/// Bring the managed Steam apps in `apps` in line with `selected`. Apps the
/// user added keep their settings; only a missing Steam cover is filled in.
/// Returns whether anything changed.
pub fn reconcile(
    apps: &mut Vec<Value>,
    selected: &[&Game],
    settings: &Settings,
    covers: &HashMap<u32, PathBuf>,
) -> bool {
    let games: Vec<&Game> = selected
        .iter()
        .copied()
        .filter(|g| importable(g, settings.include_tools) && !is_excluded(g, &settings.exclusions))
        .collect();
    let by_id: HashMap<String, &Game> = games.iter().map(|g| (g.appid.to_string(), *g)).collect();
    let source = settings.source();
    let remove_missing = settings.removes_missing();
    let mut changed = false;
    let mut seen = BTreeSet::new();
    let mut index = 0;
    while index < apps.len() {
        let app = &mut apps[index];
        let steam_id = id_of(app);
        let parsed = app_id_of(app);
        let managed = app.get("steam-managed").and_then(Value::as_str) == Some("auto");
        // Early imports carry the Steam UUID but predate the marker.
        let legacy = app.get("steam-managed").is_none()
            && parsed.is_some_and(|id| {
                app.get("uuid")
                    .and_then(Value::as_str)
                    .is_some_and(|uuid| uuid.eq_ignore_ascii_case(&canonical_uuid(id)))
            });
        if steam_id.is_empty() || (!managed && !legacy) {
            if let Some(game) = by_id.get(&steam_id)
                && let Some(cover) = covers.get(&game.appid)
            {
                let image = app.get("image-path").and_then(Value::as_str).unwrap_or("");
                let previous = app
                    .get("steam-artwork-client-path")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if image.is_empty()
                    || image == FALLBACK_IMAGE
                    || (!previous.is_empty() && image == previous)
                {
                    let before = app.clone();
                    let cover = generic(cover);
                    app["image-path"] = json!(cover);
                    app["steam-artwork-client-path"] = json!(cover);
                    app["steam-artwork-client-compatible"] = json!(true);
                    changed |= *app != before;
                }
            }
            index += 1;
            continue;
        }
        let name = app
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if excluded(&steam_id, &name, &settings.exclusions) || seen.contains(&steam_id) {
            apps.remove(index);
            changed = true;
            continue;
        }
        seen.insert(steam_id.clone());
        let before = app.clone();
        if let Some(game) = by_id.get(&steam_id) {
            update(
                app,
                game,
                source,
                covers.get(&game.appid).map(PathBuf::as_path),
            );
        } else {
            if let Some(id) = parsed {
                app["uuid"] = json!(canonical_uuid(id));
                app["steam-id"] = json!(id.to_string());
                app["steam-managed"] = json!("auto");
            }
            if remove_missing {
                apps.remove(index);
                changed = true;
                continue;
            }
        }
        changed |= *app != before;
        index += 1;
    }
    for game in games {
        let key = game.appid.to_string();
        if seen.contains(&key) || apps.iter().any(|app| id_of(app) == key) {
            continue;
        }
        let mut app = json!({});
        update(
            &mut app,
            game,
            source,
            covers.get(&game.appid).map(PathBuf::as_path),
        );
        apps.push(app);
        changed = true;
    }
    changed
}

/// A process as the Steam tracker sees it.
#[derive(Clone, Debug)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    /// Creation time, which tells a reused process ID apart.
    pub started: u64,
    pub name: String,
}
/// What the tracker knows about a game started through Steam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tracked {
    /// No game process yet.
    Waiting,
    Running,
    /// Its processes have all exited.
    Exited,
    /// None appeared in time; the stream stays until the user ends it.
    Unknown,
}
/// Finds the processes of a game Steam starts: new processes whose program
/// lies in the game's install folder, and their children.
pub struct Tracker {
    baseline: HashMap<u32, u64>,
    install_dir: String,
    deadline: std::time::Instant,
    /// pid → creation time.
    pub tracked: BTreeMap<u32, u64>,
    checked: BTreeSet<(u32, u64)>,
    seen: bool,
    /// Since when no tracked process is left, and how long that may last:
    /// a game that restarts itself through Steam has none for a moment.
    empty_since: Option<std::time::Instant>,
    grace: std::time::Duration,
}
/// Children a game starts that are not the game: a browser it opens, a store
/// client. Following them kept the app running after the game, and quitting
/// the app closed them.
const NOT_THE_GAME: &[&str] = &[
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "opera.exe",
    "brave.exe",
    "iexplore.exe",
    "upc.exe",
    "ubisoftconnect.exe",
    "epicgameslauncher.exe",
    "eadesktop.exe",
    "battle.net.exe",
    "galaxyclient.exe",
    "werfault.exe",
];
const STEAM_PROCESSES: &[&str] = &[
    "steam.exe",
    "steamwebhelper.exe",
    "steamservice.exe",
    "steamerrorreporter.exe",
    "steamerrorreporter64.exe",
    "gameoverlayui.exe",
    "gameoverlayui64.exe",
];
impl Tracker {
    /// `before` is the process list taken just before the launch.
    pub fn new(before: &[Process], install_dir: &str, wait: std::time::Duration) -> Self {
        Self {
            baseline: before.iter().map(|p| (p.pid, p.started)).collect(),
            install_dir: normalized(install_dir),
            deadline: std::time::Instant::now() + wait,
            tracked: BTreeMap::new(),
            checked: BTreeSet::new(),
            seen: false,
            empty_since: None,
            grace: std::time::Duration::from_secs(10),
        }
    }
    /// How long the game may have no process before it counts as exited.
    pub fn with_exit_grace(mut self, grace: std::time::Duration) -> Self {
        self.grace = grace;
        self
    }
    /// Update from the current process list; `image` gives a process's full
    /// program path (asked once per new process).
    pub fn update(
        &mut self,
        now: &[Process],
        mut image: impl FnMut(u32) -> Option<String>,
    ) -> Tracked {
        let alive: HashMap<u32, u64> = now.iter().map(|p| (p.pid, p.started)).collect();
        self.tracked
            .retain(|pid, started| alive.get(pid) == Some(started));
        loop {
            let mut added = false;
            for process in now {
                let key = (process.pid, process.started);
                if self.tracked.contains_key(&process.pid)
                    || self.baseline.get(&process.pid) == Some(&process.started)
                    || STEAM_PROCESSES.contains(&process.name.to_ascii_lowercase().as_str())
                {
                    continue;
                }
                let child = self.tracked.contains_key(&process.parent)
                    && !NOT_THE_GAME.contains(&process.name.to_ascii_lowercase().as_str());
                let inside = !child && !self.checked.contains(&key) && {
                    self.checked.insert(key);
                    image(process.pid).is_some_and(|path| {
                        let path = normalized(&path);
                        !self.install_dir.is_empty()
                            && path.starts_with(&self.install_dir)
                            && path[self.install_dir.len()..].starts_with('\\')
                    })
                };
                if child || inside {
                    self.tracked.insert(process.pid, process.started);
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
        if !self.tracked.is_empty() {
            self.seen = true;
            self.empty_since = None;
            Tracked::Running
        } else if self.seen {
            let since = *self.empty_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= self.grace {
                Tracked::Exited
            } else {
                Tracked::Running
            }
        } else if std::time::Instant::now() >= self.deadline {
            Tracked::Unknown
        } else {
            Tracked::Waiting
        }
    }
}
fn normalized(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_vdf_reads_library_folders_and_manifests() {
        let folders = Vdf::parse(
            r#""libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"apps" { "228980" "1" }
	}
	// a comment
	"1" { "path" "D:\\SteamLibrary" }
	}
}"#,
        );
        let paths: Vec<_> = folders
            .get("libraryfolders")
            .unwrap()
            .entries()
            .iter()
            .filter_map(|(_, f)| f.get("path").and_then(Vdf::text))
            .collect();
        assert_eq!(
            paths,
            ["C:\\Program Files (x86)\\Steam", "D:\\SteamLibrary"]
        );
        let manifest = Vdf::parse(
            "\"AppState\" { \"appid\" \"570\" \"name\" \"Dota 2\" \"installdir\" \"dota 2 beta\" }",
        );
        assert_eq!(
            manifest.at(&["AppState", "appid"]).and_then(Vdf::number),
            Some(570)
        );
        assert_eq!(
            manifest.at(&["appstate", "NAME"]).and_then(Vdf::text),
            Some("Dota 2")
        );
    }
    fn binary_entry(appid: u32, kv: &[u8]) -> Vec<u8> {
        let mut entry = appid.to_le_bytes().to_vec();
        entry.extend(((60 + kv.len()) as u32).to_le_bytes());
        entry.extend([0; 60]);
        entry.extend(kv);
        entry
    }
    #[test]
    fn binary_appinfo_gives_names_and_types() {
        // Format 29: keys are indices into a string table.
        let keys = ["appinfo", "common", "name", "type"];
        let mut kv = vec![0x00];
        kv.extend(0u32.to_le_bytes());
        kv.push(0x00);
        kv.extend(1u32.to_le_bytes());
        kv.push(0x01);
        kv.extend(2u32.to_le_bytes());
        kv.extend(b"Half-Life\0");
        kv.push(0x01);
        kv.extend(3u32.to_le_bytes());
        kv.extend(b"Game\0");
        kv.extend([0x08, 0x08, 0x08]);
        let mut data = 0x0756_4429u32.to_le_bytes().to_vec();
        data.extend(1u32.to_le_bytes());
        data.extend([0; 8]);
        data.extend(binary_entry(70, &kv));
        data.extend(binary_entry(80, &kv));
        data.extend(0u32.to_le_bytes());
        let table = data.len() as u64;
        data[8..16].copy_from_slice(&table.to_le_bytes());
        data.extend((keys.len() as u32).to_le_bytes());
        for key in keys {
            data.extend(key.as_bytes());
            data.push(0);
        }
        let found = appinfo(&data, &BTreeSet::from([70]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[&70], ("Half-Life".into(), "Game".into()));
        assert!(appinfo(&data[..40], &BTreeSet::from([70])).is_empty());
    }
    fn game(appid: u32, name: &str, last_played: u64) -> Game {
        Game {
            appid,
            name: name.into(),
            installed: true,
            last_played,
            install_dir: PathBuf::from(format!("C:/Steam/steamapps/common/{name}")),
            ..Default::default()
        }
    }
    #[test]
    fn selection_and_identity_follow_vibepollo() {
        assert_eq!(canonical_uuid(570), "53544541-4d00-5000-8000-00000000023a");
        assert_eq!(app_id_of(&json!({"uuid": canonical_uuid(570)})), Some(570));
        assert_eq!(app_id_of(&json!({"steam-id": "steam-730"})), Some(730));
        let games = vec![
            game(1, "Old", 100),
            game(2, "New", 300),
            game(3, "Never", 0),
            game(228980, "Steamworks Common Redistributables", 400),
            Game {
                app_type: "Tool".into(),
                ..game(4, "SDK", 500)
            },
        ];
        let mut settings = Settings::from_config(&Config::default());
        settings.recent_max_age_days = 0;
        let ids = |s: &Settings| {
            select(&games, s, 1000)
                .iter()
                .map(|g| g.appid)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&settings), [2, 1]);
        settings.recent_games = 1;
        assert_eq!(ids(&settings), [2]);
        settings.recent_games = 10;
        settings.exclusions = id_names(r#"[{"id":"","name":"new"}]"#);
        assert_eq!(ids(&settings), [1]);
        settings.sync_all_installed = true;
        settings.exclusions = id_names("1");
        assert_eq!(ids(&settings), [2, 3]);
        settings.include_tools = true;
        assert_eq!(ids(&settings), [2, 3, 228980, 4]);
    }
    #[test]
    fn reconcile_owns_only_managed_apps() {
        let mut settings = Settings::from_config(&Config::default());
        settings.sync_all_installed = true;
        settings.remove_uninstalled = true;
        let dota = game(570, "Dota 2", 10);
        let cs = game(730, "Counter-Strike 2", 20);
        let mut apps = vec![
            json!({"name":"Desktop"}),
            // Added by hand: only its missing cover is filled in.
            json!({"name":"My CS","steam-id":"730","cmd":"custom","image-path":""}),
            // Managed but no longer installed.
            json!({"name":"Gone","uuid":canonical_uuid(440),"steam-id":"440","steam-managed":"auto"}),
        ];
        let covers = HashMap::from([(730, PathBuf::from("C:\\covers\\steam_730.png"))]);
        assert!(reconcile(&mut apps, &[&dota, &cs], &settings, &covers));
        let names: Vec<_> = apps.iter().map(|a| a["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Desktop", "My CS", "Dota 2"]);
        assert_eq!(apps[1]["cmd"], "custom");
        assert_eq!(apps[1]["image-path"], "C:/covers/steam_730.png");
        assert_eq!(apps[2]["uuid"], canonical_uuid(570));
        assert_eq!(apps[2]["cmd"], "cmd /c start \"\" steam://rungameid/570");
        assert_eq!(apps[2]["image-path"], FALLBACK_IMAGE);
        assert_eq!(
            apps[2]["steam-install-dir"],
            "C:/Steam/steamapps/common/Dota 2"
        );
        // A second pass changes nothing.
        assert!(!reconcile(&mut apps, &[&dota, &cs], &settings, &covers));
        // A cover the user picked stays.
        apps[2]["image-path"] = json!("C:/mine.png");
        assert!(!reconcile(&mut apps, &[&dota, &cs], &settings, &covers));
        assert_eq!(apps[2]["image-path"], "C:/mine.png");
    }
    #[test]
    fn tracker_follows_the_game_and_its_children() {
        let process = |pid, parent, name: &str| Process {
            pid,
            parent,
            started: u64::from(pid) * 10,
            name: name.into(),
        };
        let before = [process(1, 0, "explorer.exe"), process(2, 1, "steam.exe")];
        let mut tracker = Tracker::new(
            &before,
            "C:\\Games\\Portal",
            std::time::Duration::from_secs(15),
        )
        .with_exit_grace(std::time::Duration::ZERO);
        let images = |pid| match pid {
            3 => Some("c:\\games\\portal\\portal.exe".to_owned()),
            5 => Some("C:\\Games\\Portal2\\other.exe".to_owned()),
            _ => None,
        };
        assert_eq!(tracker.update(&before, images), Tracked::Waiting);
        let launched = [
            process(1, 0, "explorer.exe"),
            process(2, 1, "steam.exe"),
            process(3, 2, "portal.exe"),
            process(4, 3, "crashhandler.exe"),
            process(5, 1, "other.exe"),
        ];
        assert_eq!(tracker.update(&launched, images), Tracked::Running);
        assert_eq!(tracker.tracked.keys().copied().collect::<Vec<_>>(), [3, 4]);
        assert_eq!(tracker.update(&before, images), Tracked::Exited);
    }
    #[test]
    fn a_game_restarting_through_steam_and_a_browser_it_opens_do_not_end_or_extend_it() {
        let process = |pid, parent, name: &str| Process {
            pid,
            parent,
            started: u64::from(pid) * 10,
            name: name.into(),
        };
        let before = [process(1, 0, "explorer.exe"), process(2, 1, "steam.exe")];
        let images = |pid| match pid {
            3 | 6 => Some("c:\\games\\portal\\portal.exe".to_owned()),
            _ => None,
        };
        let mut tracker = Tracker::new(
            &before,
            "C:\\Games\\Portal",
            std::time::Duration::from_secs(15),
        );
        let running = [
            process(1, 0, "explorer.exe"),
            process(2, 1, "steam.exe"),
            process(3, 2, "portal.exe"),
            process(4, 3, "chrome.exe"),
        ];
        assert_eq!(tracker.update(&running, images), Tracked::Running);
        // The browser the game opened is not the game.
        assert_eq!(tracker.tracked.keys().copied().collect::<Vec<_>>(), [3]);
        // Restarting through Steam: no game process for a moment.
        assert_eq!(tracker.update(&before, images), Tracked::Running);
        let restarted = [
            process(1, 0, "explorer.exe"),
            process(2, 1, "steam.exe"),
            process(6, 2, "portal.exe"),
        ];
        assert_eq!(tracker.update(&restarted, images), Tracked::Running);
        assert_eq!(tracker.tracked.keys().copied().collect::<Vec<_>>(), [6]);
    }
}
