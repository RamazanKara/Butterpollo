//! Playnite: the plugin's messages and the apps a sync keeps, as in
//! Vibepollo 2.0.
//!
//! The Playnite plugin (`SunshinePlaynite`) serves a named pipe and sends
//! one JSON message per line. A sync matches the library's apps against
//! Playnite's games and adds the recently played, chosen category, chosen
//! plugin or all installed games as apps marked `playnite-managed: "auto"`.
use crate::config::Config;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::PathBuf,
    time::{Duration, Instant},
};

/// A game as the plugin describes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Game {
    pub id: String,
    pub name: String,
    pub exe: String,
    pub args: String,
    pub working_dir: String,
    pub install_dir: String,
    pub categories: Vec<String>,
    pub plugin_id: String,
    pub plugin_name: String,
    pub playtime_minutes: u64,
    /// Unix seconds, if Playnite has a valid last played time.
    pub last_played: Option<i64>,
    pub box_art_path: String,
    pub icon_path: String,
    pub installed: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    SnapshotStart,
    SnapshotComplete,
    Plugins(Vec<(String, String)>),
    Categories(Vec<(String, String)>),
    Games(Vec<Game>),
    /// gameStarted, gameStopped, stopRequested or playniteExiting.
    Status {
        name: String,
        id: String,
        install_dir: String,
        exe: String,
    },
    /// The answer to a command that asked for one (`set-cover`).
    CommandResult {
        command: String,
        request_id: String,
        success: bool,
        error: String,
    },
    Other,
}
fn text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}
fn pairs(value: Option<&Value>) -> Vec<(String, String)> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| (text(item, "id"), text(item, "name")))
                .filter(|(id, name)| !id.is_empty() || !name.is_empty())
                .collect()
        })
        .unwrap_or_default()
}
/// One line from the plugin (a UTF-8 BOM is allowed).
pub fn parse(line: &str) -> Message {
    let Ok(message) = serde_json::from_str::<Value>(line.trim_start_matches('\u{feff}').trim())
    else {
        return Message::Other;
    };
    match message.get("type").and_then(Value::as_str).unwrap_or("") {
        "snapshotStart" => Message::SnapshotStart,
        "snapshotComplete" => Message::SnapshotComplete,
        "plugins" => Message::Plugins(pairs(message.get("payload"))),
        "categories" => Message::Categories(pairs(message.get("payload"))),
        "games" => Message::Games(
            message
                .get("payload")
                .and_then(Value::as_array)
                .map(|games| games.iter().filter_map(game).collect())
                .unwrap_or_default(),
        ),
        "status" => {
            let status = message.get("status").cloned().unwrap_or_default();
            Message::Status {
                name: text(&status, "name"),
                id: text(&status, "id"),
                install_dir: text(&status, "installDir"),
                exe: text(&status, "exe"),
            }
        }
        "commandResult" => Message::CommandResult {
            command: text(&message, "command"),
            request_id: text(&message, "requestId"),
            success: message.get("success").and_then(Value::as_bool) == Some(true),
            error: text(&message, "error"),
        },
        _ => Message::Other,
    }
}
fn game(value: &Value) -> Option<Game> {
    let id = text(value, "id");
    if id.is_empty() {
        return None;
    }
    let flag = |key: &str| value.get(key).map(|v| v.as_bool().unwrap_or(false));
    // Without either flag the game counts as installed, as in Vibepollo.
    let installed = match (flag("installed"), flag("isInstalled")) {
        (None, None) => true,
        (a, b) => a.unwrap_or(false) || b.unwrap_or(false),
    };
    Some(Game {
        name: text(value, "name"),
        exe: text(value, "exe"),
        args: text(value, "args"),
        working_dir: text(value, "workingDir"),
        install_dir: text(value, "installDir"),
        categories: value
            .get("categories")
            .and_then(Value::as_array)
            .map(|c| {
                c.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        plugin_id: text(value, "pluginId"),
        plugin_name: text(value, "pluginName"),
        playtime_minutes: text(value, "playtimeMinutes").parse().unwrap_or(0),
        last_played: parse_time(&text(value, "lastPlayed")),
        box_art_path: text(value, "boxArtPath"),
        icon_path: text(value, "iconPath"),
        installed,
        id,
    })
}
/// An ISO 8601 time (`2026-08-29T17:49:56.1234567+02:00`, `...Z`) as Unix
/// seconds.
pub fn parse_time(value: &str) -> Option<i64> {
    let value = value.trim();
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = value.get(range)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let separators = value.as_bytes();
    if separators.len() < 19
        || separators[4] != b'-'
        || separators[7] != b'-'
        || !matches!(separators[10], b'T' | b't' | b' ')
        || separators[13] != b':'
        || separators[16] != b':'
    {
        return None;
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut rest = &value[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        rest = fraction.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest.as_bytes().first() {
        None | Some(b'Z' | b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let zone = &rest[1..];
            let (h, m) = zone.split_once(':')?;
            let minutes = h.parse::<i64>().ok()? * 60 + m.parse::<i64>().ok()?;
            if *sign == b'+' { minutes } else { -minutes }
        }
        _ => return None,
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hour * 3600 + minute * 60 + second - offset * 60)
}
/// Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_time(seconds: i64) -> String {
    let days = seconds.div_euclid(86400);
    let rest = seconds.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// The `playnite_*` settings, with Vibepollo's defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub auto_sync: bool,
    pub sync_all_installed: bool,
    pub recent_games: usize,
    pub recent_max_age_days: i64,
    pub delete_after_days: i64,
    pub remove_uninstalled: bool,
    pub require_replacement: bool,
    /// Category names.
    pub sync_categories: Vec<String>,
    pub exclude_categories: Vec<String>,
    /// Plugin and game ids.
    pub sync_plugins: Vec<String>,
    pub exclude_plugins: Vec<String>,
    pub exclude_games: Vec<String>,
    pub focus_attempts: i64,
    pub focus_timeout_secs: i64,
    pub focus_exit_on_first: bool,
    pub fullscreen_entry: bool,
}
impl Settings {
    pub fn from_config(config: &Config) -> Self {
        let count = |key: &str, default: i64| config.integer(key, default).max(0);
        // Category lists hold names, the others ids; a plain string is
        // whichever the list holds.
        let list = |key: &str, by_name: bool| -> Vec<String> {
            crate::steam::id_names(config.get(key, ""))
                .into_iter()
                .map(|(id, name)| {
                    if by_name && !name.is_empty() {
                        name
                    } else {
                        id
                    }
                })
                .filter(|value| !value.is_empty())
                .collect()
        };
        Self {
            enabled: config.boolean("playnite_enabled", true),
            auto_sync: config.boolean("playnite_auto_sync", true),
            sync_all_installed: config.boolean("playnite_sync_all_installed", false),
            recent_games: count("playnite_recent_games", 10) as usize,
            recent_max_age_days: count("playnite_recent_max_age_days", 30),
            delete_after_days: count("playnite_autosync_delete_after_days", 14),
            remove_uninstalled: config.boolean("playnite_autosync_remove_uninstalled", true),
            require_replacement: config.boolean("playnite_autosync_require_replacement", true),
            sync_categories: list("playnite_sync_categories", true),
            exclude_categories: list("playnite_exclude_categories", true),
            sync_plugins: list("playnite_sync_plugins", false),
            exclude_plugins: list("playnite_exclude_plugins", false),
            exclude_games: list("playnite_exclude_games", false),
            focus_attempts: count("playnite_focus_attempts", 3),
            focus_timeout_secs: count("playnite_focus_timeout_secs", 15),
            focus_exit_on_first: config.boolean("playnite_focus_exit_on_first", false),
            fullscreen_entry: config.boolean("playnite_fullscreen_entry_enabled", false),
        }
    }
}

/// Bringing a started game, or the fullscreen menu, to the front, as
/// Vibepollo's launcher does with the `playnite_focus_*` settings: once a
/// second for `focus_timeout_secs`, until the window was confirmed in front
/// `focus_attempts` times (once with `focus_exit_on_first`). Launchers and
/// splash screens take the foreground back while a game starts, so a single
/// success is not always the end of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Focus {
    left: i64,
    deadline: Instant,
    next: Instant,
}
impl Focus {
    pub const INTERVAL: Duration = Duration::from_secs(1);
    /// Start focusing at `now`; None when the settings turn it off.
    pub fn arm(settings: &Settings, now: Instant) -> Option<Self> {
        if settings.focus_attempts <= 0 || settings.focus_timeout_secs <= 0 {
            return None;
        }
        Some(Self {
            left: if settings.focus_exit_on_first {
                1
            } else {
                settings.focus_attempts
            },
            deadline: now + Duration::from_secs(settings.focus_timeout_secs as u64),
            next: now,
        })
    }
    /// Whether to check the window, and bring it forward, now.
    pub fn due(&self, now: Instant) -> bool {
        now >= self.next && !self.finished(now)
    }
    pub fn finished(&self, now: Instant) -> bool {
        self.left <= 0 || now >= self.deadline
    }
    /// Record a check at `now`; `focused` when the window was in front.
    pub fn checked(&mut self, now: Instant, focused: bool) {
        if focused {
            self.left -= 1;
        }
        self.next = now + Self::INTERVAL;
    }
}

const RECENT: u8 = 1;
const CATEGORY: u8 = 2;
const PLUGIN: u8 = 4;
const INSTALLED: u8 = 8;
fn source_label(flags: u8) -> String {
    let parts: Vec<&str> = [
        (RECENT, "recent"),
        (CATEGORY, "category"),
        (PLUGIN, "plugin"),
        (INSTALLED, "installed"),
    ]
    .iter()
    .filter(|(bit, _)| flags & bit != 0)
    .map(|(_, name)| *name)
    .collect();
    if parts.is_empty() {
        "unknown".into()
    } else {
        parts.join("+")
    }
}
fn key(id: &str) -> String {
    id.to_ascii_lowercase()
}
fn path_key(path: &str) -> String {
    path.replace('"', "").replace('/', "\\").to_lowercase()
}
/// Match a game or emulator after its launcher hands it off. Store clients
/// remaining open do not prove that the game is still running.
pub fn game_process(path: &str, install_dir: &str, exe: &str) -> bool {
    let path = path_key(path);
    let name = path.rsplit('\\').next().unwrap_or("");
    if matches!(
        name,
        "steam.exe"
            | "steamwebhelper.exe"
            | "steamservice.exe"
            | "gameoverlayui.exe"
            | "epicgameslauncher.exe"
            | "eadesktop.exe"
            | "upc.exe"
            | "ubisoftconnect.exe"
            | "battle.net.exe"
            | "galaxyclient.exe"
            | "playnite.desktopapp.exe"
            | "playnite.fullscreenapp.exe"
    ) {
        return false;
    }
    let folder = path_key(install_dir);
    let folder = folder.trim_end_matches('\\');
    (!folder.is_empty()
        && path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('\\')))
        || (!exe.is_empty() && path == path_key(exe))
}
fn name_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
fn command_program(command: &str) -> String {
    let command = command.trim_start_matches([' ', '\t']);
    let program = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => command.split([' ', '\t']).next().unwrap_or(""),
    };
    path_key(program)
}
fn field<'a>(app: &'a Value, name: &str) -> &'a str {
    app.get(name).and_then(Value::as_str).unwrap_or("")
}
/// Vibepollo's UUID for a Playnite app: the game id in capitals.
pub fn app_uuid(id: &str) -> String {
    id.to_ascii_uppercase()
}
/// Converted cover and icon files for a game, by lower-case id.
pub type Artwork = HashMap<String, (Option<PathBuf>, Option<PathBuf>)>;
fn apply_game(app: &mut Value, game: &Game, artwork: &Artwork) {
    if !app.is_object() {
        *app = json!({});
    }
    let fields = app.as_object_mut().unwrap();
    if !game.name.is_empty() {
        fields.insert("name".into(), json!(game.name));
    }
    fields.insert("playnite-id".into(), json!(game.id));
    // Playnite starts the game; the app has no command of its own.
    fields.remove("cmd");
    fields.remove("working-dir");
    for (name, value) in [
        ("playnite-plugin-id", &game.plugin_id),
        ("playnite-plugin-name", &game.plugin_name),
    ] {
        if value.is_empty() {
            fields.remove(name);
        } else {
            fields.insert(name.into(), json!(value));
        }
    }
    let (cover, icon) = artwork.get(&key(&game.id)).cloned().unwrap_or_default();
    if let Some(cover) = cover {
        fields.insert(
            "image-path".into(),
            json!(cover.to_string_lossy().replace('\\', "/")),
        );
    }
    match icon {
        Some(icon) => {
            fields.insert(
                "playnite-icon-path".into(),
                json!(icon.to_string_lossy().replace('\\', "/")),
            );
        }
        None => {
            fields.remove("playnite-icon-path");
        }
    }
    fields.insert("uuid".into(), json!(app_uuid(&game.id)));
}
fn excluded(
    game: &Game,
    ids: &HashSet<String>,
    categories: &HashSet<String>,
    plugins: &HashSet<String>,
) -> bool {
    ids.contains(&key(&game.id))
        || game
            .categories
            .iter()
            .any(|c| categories.contains(&c.to_lowercase()))
        || (!game.plugin_id.is_empty() && plugins.contains(&key(&game.plugin_id)))
}
/// Bring the library in line with Playnite's games. With `manage` (auto
/// sync) the selection adds and removes managed apps; without it only the
/// apps already linked to a game get its current name and artwork. Returns
/// whether anything changed.
pub fn reconcile(
    apps: &mut Vec<Value>,
    games: &[Game],
    settings: &Settings,
    now: i64,
    artwork: &Artwork,
    manage: bool,
) -> bool {
    let before = apps.clone();
    if manage {
        let mut seen = HashSet::new();
        apps.retain(|app| {
            let id = field(app, "playnite-id");
            field(app, "playnite-managed") != "auto" || id.is_empty() || seen.insert(key(id))
        });
    }
    let installed: Vec<&Game> = games.iter().filter(|g| g.installed).collect();
    let uninstalled: HashSet<String> = games
        .iter()
        .filter(|g| !g.installed)
        .map(|g| key(&g.id))
        .collect();
    let lower = |values: &[String]| {
        values
            .iter()
            .map(|v| v.to_lowercase())
            .collect::<HashSet<_>>()
    };
    let (ids, categories, plugins) = (
        lower(&settings.exclude_games),
        lower(&settings.exclude_categories),
        lower(&settings.exclude_plugins),
    );
    let mut flags: HashMap<String, u8> = HashMap::new();
    let mut selected: Vec<&Game> = vec![];
    fn pick<'a>(
        game: &'a Game,
        source: u8,
        flags: &mut HashMap<String, u8>,
        selected: &mut Vec<&'a Game>,
    ) {
        let entry = flags.entry(key(&game.id)).or_insert(0);
        if *entry == 0 {
            selected.push(game);
        }
        *entry |= source;
    }
    if manage {
        if settings.recent_games > 0 {
            let mut ordered = installed.clone();
            ordered.sort_by_key(|game| std::cmp::Reverse(game.last_played));
            let cutoff = now - settings.recent_max_age_days * 86400;
            let mut taken = 0;
            for game in ordered {
                if taken >= settings.recent_games {
                    break;
                }
                if excluded(game, &ids, &categories, &plugins)
                    || (settings.recent_max_age_days > 0
                        && game.last_played.is_none_or(|at| at < cutoff))
                {
                    continue;
                }
                taken += 1;
                pick(game, RECENT, &mut flags, &mut selected);
            }
        }
        let wanted = lower(&settings.sync_categories);
        let wanted_plugins = lower(&settings.sync_plugins);
        for game in installed.iter().copied() {
            if excluded(game, &ids, &categories, &plugins) {
                continue;
            }
            if game
                .categories
                .iter()
                .any(|c| wanted.contains(&c.to_lowercase()))
            {
                pick(game, CATEGORY, &mut flags, &mut selected);
            }
            if !game.plugin_id.is_empty() && wanted_plugins.contains(&key(&game.plugin_id)) {
                pick(game, PLUGIN, &mut flags, &mut selected);
            }
            if settings.sync_all_installed {
                pick(game, INSTALLED, &mut flags, &mut selected);
            }
        }
    }
    // Match existing apps: by id against every game, then by program,
    // working folder or a name only one selected game has.
    let by_id: HashMap<String, &Game> = games.iter().map(|g| (key(&g.id), g)).collect();
    let mut by_exe = HashMap::new();
    let mut by_dir = HashMap::new();
    let mut by_name: HashMap<String, Option<&Game>> = HashMap::new();
    for game in &selected {
        if !game.exe.is_empty() {
            by_exe.insert(path_key(&game.exe), *game);
        }
        if !game.working_dir.is_empty() {
            by_dir.insert(path_key(&game.working_dir), *game);
        }
        let name = name_key(&game.name);
        if !name.is_empty() {
            by_name
                .entry(name)
                .and_modify(|found| *found = None)
                .or_insert(Some(*game));
        }
    }
    let mut matched = HashSet::new();
    for app in apps.iter_mut() {
        let id = field(app, "playnite-id").to_owned();
        if field(app, "playnite-managed") == "auto"
            && !id.is_empty()
            && uninstalled.contains(&key(&id))
        {
            continue;
        }
        let game = Some(id.as_str())
            .filter(|id| !id.is_empty())
            .and_then(|id| by_id.get(&key(id)).copied())
            .or_else(|| {
                Some(field(app, "cmd"))
                    .filter(|c| !c.is_empty())
                    .and_then(|c| by_exe.get(&command_program(c)).copied())
            })
            .or_else(|| {
                Some(field(app, "working-dir"))
                    .filter(|d| !d.is_empty())
                    .and_then(|d| by_dir.get(&path_key(d)).copied())
            })
            .or_else(|| {
                by_name
                    .get(&name_key(field(app, "name")))
                    .copied()
                    .flatten()
            });
        let Some(game) = game else {
            continue;
        };
        matched.insert(key(&game.id));
        apply_game(app, game, artwork);
        if let Some(source) = flags.get(&key(&game.id)) {
            app["playnite-source"] = json!(source_label(*source));
            app["playnite-managed"] = json!("auto");
        }
    }
    if manage {
        let selected_ids: HashSet<String> = selected.iter().map(|g| key(&g.id)).collect();
        let current: HashSet<String> = apps
            .iter()
            .filter(|app| field(app, "playnite-managed") == "auto")
            .map(|app| key(field(app, "playnite-id")))
            .filter(|id| !id.is_empty())
            .collect();
        let mut replacements = selected_ids.difference(&current).count();
        let last_played: HashMap<String, i64> = installed
            .iter()
            .filter_map(|g| g.last_played.map(|at| (key(&g.id), at)))
            .collect();
        apps.retain(|app| {
            let id = key(field(app, "playnite-id"));
            if field(app, "playnite-managed") != "auto" || id.is_empty() {
                return true;
            }
            let added = parse_time(field(app, "playnite-added-at")).unwrap_or(now);
            let expired = settings.delete_after_days > 0
                && now >= added + settings.delete_after_days * 86400
                && last_played.get(&id).is_none_or(|played| *played < added);
            let mut remove = (settings.remove_uninstalled && uninstalled.contains(&id)) || expired;
            if !remove && !selected_ids.contains(&id) {
                if !settings.sync_all_installed && field(app, "playnite-source") == "installed" {
                    remove = true;
                } else if settings.recent_games > 0
                    && settings.require_replacement
                    && replacements > 0
                {
                    replacements -= 1;
                    remove = true;
                }
            }
            !remove
        });
        for game in &selected {
            if matched.contains(&key(&game.id)) {
                continue;
            }
            let mut app = json!({});
            apply_game(&mut app, game, artwork);
            app["playnite-source"] = json!(source_label(flags[&key(&game.id)]));
            app["playnite-managed"] = json!("auto");
            app["playnite-added-at"] = json!(format_time(now));
            app["exit-timeout"] = json!(10);
            apps.push(app);
        }
    }
    *apps != before
}
/// The "Playnite (Fullscreen)" app: present while `wanted`, removed
/// otherwise. Returns whether anything changed.
pub fn fullscreen_entry(apps: &mut Vec<Value>, wanted: bool) -> bool {
    let is_entry = |app: &Value| {
        let flag = match app.get("playnite-fullscreen") {
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_i64() == Some(1),
            Some(Value::String(s)) => {
                matches!(s.to_ascii_lowercase().as_str(), "true" | "1" | "yes")
            }
            _ => false,
        };
        let cmd = field(app, "cmd").to_ascii_lowercase();
        flag || (cmd.contains("playnite-launcher") && cmd.contains("--fullscreen"))
            || field(app, "name") == "Playnite (Fullscreen)"
    };
    let present = apps.iter().any(is_entry);
    if wanted && !present {
        apps.push(json!({
            "name": "Playnite (Fullscreen)",
            "image-path": "playnite_boxart.png",
            "playnite-fullscreen": true,
            "auto-detach": true,
            "wait-all": true,
            "exit-timeout": 10,
        }));
        return true;
    }
    if !wanted && present {
        apps.retain(|app| !is_entry(app));
        return true;
    }
    false
}
/// Remove every app the sync added.
pub fn purge(apps: &mut Vec<Value>) -> usize {
    let before = apps.len();
    apps.retain(|app| field(app, "playnite-managed") != "auto");
    before - apps.len()
}
/// Category names and plugins seen in a game list, for the console.
pub fn names(games: &[Game]) -> (BTreeSet<String>, BTreeSet<(String, String)>) {
    let categories = games
        .iter()
        .flat_map(|g| g.categories.iter().cloned())
        .collect();
    let plugins = games
        .iter()
        .filter(|g| !g.plugin_id.is_empty())
        .map(|g| (g.plugin_id.clone(), g.plugin_name.clone()))
        .collect();
    (categories, plugins)
}
/// `Version: 0.4.14` from an `extension.yaml`.
pub fn plugin_version(manifest: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case("version")
            .then(|| {
                value
                    .trim()
                    .trim_matches(['"', '\''])
                    .trim_start_matches(['v', 'V'])
                    .to_owned()
            })
            .filter(|v| !v.is_empty())
    })
}
/// Whether version `a` is newer than `b` (dotted numbers).
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|p| p.trim().parse().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parts(a), parts(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn game_processes_survive_store_handoffs_without_tracking_the_store() {
        let folder = r"C:\Games\Nightfire";
        assert!(game_process(
            r"C:\Games\Nightfire\bin\game.exe",
            folder,
            "steam://run/123"
        ));
        for store in [r"C:\Steam\steam.exe", r"C:\Epic\EpicGamesLauncher.exe"] {
            assert!(!game_process(store, r"C:\", store));
        }
        assert!(!game_process(r"C:\Games\Nightfire2\game.exe", folder, ""));
    }
    #[test]
    fn game_process_paths_preserve_unicode_and_match_external_emulators() {
        assert!(game_process(
            r"C:\Çağrı\Oyun\game.exe",
            "C:/Çağrı/Oyun/",
            ""
        ));
        assert!(game_process(
            r"D:\Emulators\emu.exe",
            r"C:\ROMs",
            "D:/Emulators/emu.exe"
        ));
        assert!(!game_process(
            r"D:\Other\emu.exe",
            r"C:\ROMs",
            "D:/Emulators/emu.exe"
        ));
    }
    #[test]
    fn messages_and_times_parse_like_vibepollo() {
        assert_eq!(parse_time("1970-01-02T00:00:00Z"), Some(86400));
        assert_eq!(parse_time("2026-08-29T17:49:56Z"), Some(1788025796));
        assert_eq!(
            parse_time("2026-08-29T19:49:56.1234567+02:00"),
            Some(1788025796)
        );
        assert_eq!(parse_time("2026-08-29"), None);
        assert_eq!(format_time(1788025796), "2026-08-29T17:49:56Z");
        assert_eq!(
            parse(
                r#"{"type":"commandResult","command":"set-cover","requestId":"cover-1","success":false,"error":"Playnite rejected the cover metadata update"}"#
            ),
            Message::CommandResult {
                command: "set-cover".into(),
                request_id: "cover-1".into(),
                success: false,
                error: "Playnite rejected the cover metadata update".into(),
            }
        );
        assert!(matches!(
            parse(r#"{"type":"commandResult","requestId":"cover-2","success":true}"#),
            Message::CommandResult { success: true, .. }
        ));
        let line = "\u{feff}{\"type\":\"games\",\"payload\":[{\"id\":\"A1\",\"name\":\"Nightfire\",\"categories\":[\"Shooter\"],\"playtimeMinutes\":\"42\",\"lastPlayed\":\"2026-08-29T17:49:56Z\",\"pluginId\":\"P\"},{\"id\":\"B2\",\"installed\":false},{\"name\":\"no id\"}]}";
        let Message::Games(games) = parse(line) else {
            panic!()
        };
        assert_eq!(games.len(), 2);
        assert!(games[0].installed && !games[1].installed);
        assert_eq!(games[0].playtime_minutes, 42);
        assert_eq!(games[0].last_played, Some(1788025796));
        assert_eq!(
            parse(
                "{\"type\":\"status\",\"status\":{\"name\":\"gameStarted\",\"id\":\"A1\",\"installDir\":\"C:\\\\G\"}}"
            ),
            Message::Status {
                name: "gameStarted".into(),
                id: "A1".into(),
                install_dir: "C:\\G".into(),
                exe: String::new()
            }
        );
        assert_eq!(parse("not json"), Message::Other);
        assert_eq!(
            plugin_version("Name: x\nVersion: 0.4.14\n").as_deref(),
            Some("0.4.14")
        );
        assert!(newer("0.4.14", "0.4.9") && !newer("0.4.14", "0.4.14"));
    }
    fn game(id: &str, name: &str, last: Option<i64>) -> Game {
        Game {
            id: id.into(),
            name: name.into(),
            last_played: last,
            installed: true,
            ..Default::default()
        }
    }
    #[test]
    fn sync_selects_recent_games_and_keeps_user_apps() {
        let now = 100 * 86400;
        let mut settings = Settings::from_config(&Config::default());
        settings.recent_games = 2;
        let games = vec![
            game("aa", "Old", Some(now - 40 * 86400)),
            game("bb", "Newer", Some(now - 86400)),
            game("cc", "Newest", Some(now - 60)),
            Game {
                installed: false,
                ..game("dd", "Gone", Some(now))
            },
        ];
        let mut apps = vec![
            json!({"name": "Desktop"}),
            // Added by hand with the game's name: Vibepollo links it.
            json!({"name": "  newest ", "cmd": "C:/x.exe"}),
            json!({"name": "Gone", "playnite-id": "dd", "playnite-managed": "auto"}),
        ];
        assert!(reconcile(
            &mut apps,
            &games,
            &settings,
            now,
            &Artwork::new(),
            true
        ));
        let names: Vec<_> = apps.iter().map(|a| field(a, "name").to_owned()).collect();
        assert_eq!(names, ["Desktop", "Newest", "Newer"]);
        assert_eq!(apps[1]["uuid"], "CC");
        assert!(apps[1].get("cmd").is_none());
        assert_eq!(apps[2]["playnite-source"], "recent");
        assert_eq!(apps[2]["playnite-added-at"], format_time(now));
        assert!(!reconcile(
            &mut apps,
            &games,
            &settings,
            now,
            &Artwork::new(),
            true
        ));
        // A game played since replaces the oldest managed one.
        let mut games = games;
        games[0].last_played = Some(now);
        assert!(reconcile(
            &mut apps,
            &games,
            &settings,
            now + 1,
            &Artwork::new(),
            true
        ));
        let ids: Vec<_> = apps
            .iter()
            .map(|a| field(a, "playnite-id").to_owned())
            .collect();
        assert_eq!(ids, ["", "cc", "aa"]);
        // Without auto sync only linked apps are refreshed.
        games[0].name = "Renamed".into();
        assert!(reconcile(
            &mut apps,
            &games,
            &settings,
            now + 2,
            &Artwork::new(),
            false
        ));
        assert_eq!(apps.len(), 3);
        assert_eq!(apps[2]["name"], "Renamed");
    }
    #[test]
    fn exclusions_categories_ttl_and_the_fullscreen_entry() {
        let now = 1000 * 86400;
        let mut settings = Settings::from_config(
            &Config::parse("playnite_recent_games = 0\nplaynite_sync_categories = [{\"id\":\"1\",\"name\":\"Couch\"}]\nplaynite_exclude_games = [\"bb\"]\n").unwrap(),
        );
        assert_eq!(settings.sync_categories, ["Couch"]);
        let mut couch = game("aa", "Kart", None);
        couch.categories = vec!["couch".into()];
        let mut excluded_game = game("bb", "Party", None);
        excluded_game.categories = vec!["Couch".into()];
        let mut apps = vec![
            json!({"name":"Stale","playnite-id":"zz","playnite-managed":"auto","playnite-added-at":format_time(now - 20 * 86400)}),
        ];
        assert!(reconcile(
            &mut apps,
            &[couch, excluded_game],
            &settings,
            now,
            &Artwork::new(),
            true
        ));
        let ids: Vec<_> = apps
            .iter()
            .map(|a| field(a, "playnite-id").to_owned())
            .collect();
        assert_eq!(ids, ["aa"]);
        assert_eq!(apps[0]["playnite-source"], "category");
        settings.fullscreen_entry = true;
        assert!(fullscreen_entry(&mut apps, true));
        assert!(!fullscreen_entry(&mut apps, true));
        assert!(fullscreen_entry(&mut apps, false));
        assert_eq!(apps.len(), 1);
        assert_eq!(purge(&mut apps), 1);
    }

    fn focus_settings(attempts: i64, timeout: i64, exit_on_first: bool) -> Settings {
        let mut settings = Settings::from_config(&Config::default());
        settings.focus_attempts = attempts;
        settings.focus_timeout_secs = timeout;
        settings.focus_exit_on_first = exit_on_first;
        settings
    }
    #[test]
    fn focus_defaults_match_vibepollo_and_zero_turns_it_off() {
        let settings = Settings::from_config(&Config::default());
        assert_eq!(
            (settings.focus_attempts, settings.focus_timeout_secs),
            (3, 15)
        );
        assert!(!settings.focus_exit_on_first);
        let now = Instant::now();
        assert!(Focus::arm(&focus_settings(0, 15, false), now).is_none());
        assert!(Focus::arm(&focus_settings(3, 0, true), now).is_none());
    }
    #[test]
    fn focus_retries_once_a_second_until_confirmed_the_set_number_of_times() {
        let at = Instant::now();
        let mut focus = Focus::arm(&focus_settings(2, 15, false), at).unwrap();
        assert!(focus.due(at));
        focus.checked(at, false);
        assert!(!focus.due(at + Duration::from_millis(500)));
        let second = at + Focus::INTERVAL;
        assert!(focus.due(second));
        focus.checked(second, true);
        assert!(!focus.finished(second));
        let third = second + Focus::INTERVAL;
        focus.checked(third, true);
        assert!(focus.finished(third));
        assert!(!focus.due(third + Focus::INTERVAL));
    }
    #[test]
    fn focus_can_stop_at_the_first_success_and_always_stops_at_the_timeout() {
        let at = Instant::now();
        let mut first = Focus::arm(&focus_settings(5, 15, true), at).unwrap();
        first.checked(at, true);
        assert!(first.finished(at));
        let mut timed = Focus::arm(&focus_settings(5, 3, false), at).unwrap();
        timed.checked(at, false);
        assert!(timed.due(at + Duration::from_secs(2)));
        assert!(!timed.due(at + Duration::from_secs(3)));
        assert!(timed.finished(at + Duration::from_secs(3)));
    }
}
