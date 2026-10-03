use anyhow::{Result, bail};
use butterpollo_core::state::{App, PrepCommand};
use butterpollo_windows::process::Process;
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    time::Duration,
};
#[derive(Clone, serde::Deserialize)]
struct ClientCommand {
    cmd: String,
    #[serde(default)]
    elevated: bool,
}
/// Administrator-configured hooks run once for each connected transport, with
/// its environment saved for disconnect even if the application exits first.
pub struct ClientCommands {
    undo: Vec<ClientCommand>,
    environment: BTreeMap<String, String>,
}
impl ClientCommands {
    pub fn start(h: &crate::state::Shared, s: &butterpollo_core::session::Session) -> Result<Self> {
        if s.launch.role == butterpollo_core::session::Role::InputOnly
            || !s.launch.client.allows_commands()
            || h.current_app
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|app| !app.allow_client_commands)
        {
            return Ok(Self {
                undo: vec![],
                environment: BTreeMap::new(),
            });
        }
        let environment = h
            .current_app
            .lock()
            .unwrap()
            .as_ref()
            .map(|app| app.environment.clone());
        let mut environment = match environment {
            Some(environment) => environment,
            None => match butterpollo_windows::process::user_environment() {
                Ok(environment) => environment,
                Err(error) => {
                    tracing::info!(%error, "no signed-in user; client commands skipped");
                    return Ok(Self {
                        undo: vec![],
                        environment: BTreeMap::new(),
                    });
                }
            },
        };
        environment.insert("SUNSHINE_CLIENT_NAME".into(), s.launch.client.name.clone());
        environment.insert("SUNSHINE_CLIENT_UUID".into(), s.launch.client.uuid.clone());
        environment.insert("SUNSHINE_CLIENT_WIDTH".into(), s.config.width.to_string());
        environment.insert("SUNSHINE_CLIENT_HEIGHT".into(), s.config.height.to_string());
        environment.insert(
            "SUNSHINE_CLIENT_FPS".into(),
            if h.config
                .read()
                .unwrap()
                .boolean("envvar_compatibility_mode", false)
            {
                s.config.fps.to_string()
            } else {
                butterpollo_core::framegen::Rate(s.config.fps_millihz()).to_string()
            },
        );
        environment.insert("SUNSHINE_CLIENT_HDR".into(), s.config.hdr.to_string());
        for (key, value) in environment.clone() {
            if let Some(suffix) = key.strip_prefix("SUNSHINE_") {
                environment.insert(format!("APOLLO_{suffix}"), value);
            }
        }
        Self::with_environment(&s.launch.client.extra, environment)
    }
    fn with_environment(
        extra: &BTreeMap<String, serde_json::Value>,
        environment: BTreeMap<String, String>,
    ) -> Result<Self> {
        let parse = |key: &str| -> Result<Vec<ClientCommand>> {
            let commands: Vec<ClientCommand> = serde_json::from_value(
                extra
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([])),
            )?;
            if commands.len() > 64
                || commands
                    .iter()
                    .any(|c| c.cmd.len() > 32767 || c.cmd.contains('\0'))
            {
                bail!("client command list exceeds its limits");
            }
            Ok(commands)
        };
        // Validate both lists before any command is started.
        let commands = parse("do")?;
        let hooks = Self {
            undo: parse("undo")?,
            environment,
        };
        hooks.run(&commands);
        Ok(hooks)
    }
    fn run(&self, commands: &[ClientCommand]) {
        for command in commands.iter().filter(|c| !c.cmd.is_empty()) {
            if let Err(error) = expand(&command.cmd, &self.environment).and_then(|cmd| {
                Process::shell_detached(&cmd, None, command.elevated, &self.environment)
            }) {
                tracing::warn!(%error, "client connection command failed");
            }
        }
    }
}
impl Drop for ClientCommands {
    fn drop(&mut self) {
        self.run(&self.undo);
    }
}
pub struct RunningApp {
    pub id: u32,
    pub name: String,
    pub uuid: String,
    pub child: Option<Process>,
    pub owner: String,
    pub generation: String,
    undo: Vec<PrepCommand>,
    working: String,
    environment: BTreeMap<String, String>,
    started: std::time::Instant,
    auto_detach: bool,
    wait_all: bool,
    detached: bool,
    pub allow_client_commands: bool,
    terminate_on_pause: bool,
    connected: bool,
    state_events: Option<std::sync::mpsc::Sender<bool>>,
    exit_timeout: Duration,
    /// An app launched before anyone signed in, started once a user does.
    pub deferred: Option<(App, HashMap<String, String>)>,
    deferred_check: std::time::Instant,
    /// A game Steam starts, followed through its install folder.
    steam: Option<butterpollo_core::steam::Tracker>,
    steam_check: std::time::Instant,
}
/// The folder of the program an app starts, as Vibepollo uses when the app
/// has no working directory: games often load files relative to it.
fn inferred_working_dir(command: &str) -> String {
    let target = butterpollo_windows::process::command_target(command);
    if target.contains("://") {
        return String::new();
    }
    let path = Path::new(target);
    path.is_absolute()
        .then(|| path.parent())
        .flatten()
        .filter(|parent| parent.is_dir())
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default()
}
fn directory(s: &str) -> Option<&Path> {
    if s.is_empty() {
        None
    } else {
        Some(Path::new(s))
    }
}
impl RunningApp {
    pub fn with_environment(app: &App, environment: BTreeMap<String, String>) -> Result<Self> {
        let mut running = Self {
            id: app.id(),
            name: app.name.clone(),
            uuid: app
                .extra
                .get("uuid")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            child: None,
            owner: environment
                .get("SUNSHINE_CLIENT_UUID")
                .cloned()
                .unwrap_or_default(),
            generation: uuid::Uuid::new_v4().to_string(),
            undo: vec![],
            working: if app.working_dir.trim().is_empty() {
                inferred_working_dir(&app.cmd)
            } else {
                app.working_dir.clone()
            },
            environment,
            started: std::time::Instant::now(),
            auto_detach: app_bool(app, "auto-detach", true),
            wait_all: app_bool(app, "wait-all", true),
            detached: false,
            allow_client_commands: app_bool(app, "allow-client-commands", true),
            terminate_on_pause: app_bool(app, "terminate-on-pause", false),
            connected: false,
            state_events: None,
            deferred: None,
            deferred_check: std::time::Instant::now(),
            steam: None,
            steam_check: std::time::Instant::now(),
            exit_timeout: Duration::from_secs(
                app.extra
                    .get("exit-timeout")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(10)
                    .min(300),
            ),
        };
        let commands: Vec<PrepCommand> = serde_json::from_value(
            app.extra
                .get("state-cmd")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        )?;
        if !commands.is_empty() {
            let (sender, receiver) = std::sync::mpsc::channel::<bool>();
            let mut environment = running.environment.clone();
            let working = running.working.clone();
            std::thread::Builder::new()
                .name("app-state-commands".into())
                .spawn(move || {
                    for active in receiver {
                        environment.insert(
                            "APOLLO_APP_STATUS".into(),
                            if active { "RESUMING" } else { "PAUSING" }.into(),
                        );
                        for command in &commands {
                            let cmd = if active { &command.r#do } else { &command.undo };
                            if cmd.is_empty() {
                                continue;
                            }
                            match expand(cmd, &environment)
                                .and_then(|cmd| {
                                    Process::shell(
                                        &cmd,
                                        directory(&working),
                                        command.elevated,
                                        &environment,
                                    )
                                })
                                .and_then(|process| process.wait(Duration::from_secs(120)))
                            {
                                Ok(0) => {}
                                Ok(code) => {
                                    tracing::warn!(code, "application state command failed");
                                    break;
                                }
                                Err(error) => {
                                    tracing::warn!(%error,"application state command failed");
                                    break;
                                }
                            }
                        }
                    }
                })?;
            running.state_events = Some(sender);
        }
        for prep in &app.prep {
            if !prep.r#do.is_empty() {
                let child = Process::shell(
                    &prep.r#do,
                    directory(&running.working),
                    prep.elevated,
                    &running.environment,
                )?;
                let code = child.wait(Duration::from_secs(120))?;
                if code != 0 {
                    bail!("preparation command exited with {code}");
                }
            }
            running.undo.push(prep.clone());
        }
        // Steam starts the game itself, outside the app's process group; the
        // processes that appear in its install folder are the game.
        if let Some(folder) = app
            .extra
            .get("steam-install-dir")
            .and_then(serde_json::Value::as_str)
            .filter(|folder| !folder.trim().is_empty())
            .filter(|_| {
                app.extra
                    .get("steam-id")
                    .is_some_and(serde_json::Value::is_string)
            })
        {
            match butterpollo_windows::process::processes() {
                Ok(before) => {
                    running.steam = Some(butterpollo_core::steam::Tracker::new(
                        &before,
                        folder,
                        Duration::from_secs(15),
                    ))
                }
                Err(error) => tracing::warn!(%error, "Steam game tracking unavailable"),
            }
        }
        if let Some(commands) = app
            .extra
            .get("detached")
            .and_then(serde_json::Value::as_array)
        {
            for command in commands {
                let command = command
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("detached command must be a string"))?;
                Process::shell_detached(
                    command,
                    directory(&running.working),
                    app_bool(app, "elevated", false),
                    &running.environment,
                )?;
            }
        }
        if !app.cmd.is_empty() {
            let command = if let Some(output) = app
                .extra
                .get("output")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
            {
                if output.contains(['\0', '\r', '\n', '"', '%', '!']) {
                    bail!("application output path contains shell metacharacters");
                }
                let output = if output == "null" { "NUL" } else { output };
                if output != "NUL"
                    && let Some(parent) = Path::new(output)
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                {
                    std::fs::create_dir_all(parent)?;
                }
                format!("({}) 1>>\"{}\" 2>&1", app.cmd, output)
            } else {
                app.cmd.clone()
            };
            running.child = Some(Process::shell(
                &command,
                directory(&running.working),
                app_bool(app, "elevated", false),
                &running.environment,
            )?);
        }
        Ok(running)
    }
    pub fn stop(&mut self) {
        self.state_events.take();
        if let Some(tracker) = self.steam.take() {
            butterpollo_windows::process::stop_processes(&tracker.tracked, self.exit_timeout);
        }
        if let Some(child) = self.child.take()
            && let Err(error) = child.stop_graceful(self.exit_timeout)
        {
            tracing::warn!(%error, "application could not exit gracefully");
            let _ = child.stop();
        }
        for prep in self.undo.drain(..).rev() {
            if !prep.undo.is_empty() {
                match Process::shell(
                    &prep.undo,
                    directory(&self.working),
                    prep.elevated,
                    &self.environment,
                )
                .and_then(|p| p.wait(Duration::from_secs(20)))
                {
                    Ok(0) => {}
                    Ok(code) => tracing::warn!(code, "undo command failed"),
                    Err(e) => tracing::warn!(error=%e,"undo command failed"),
                }
            }
        }
    }
    /// The deferred app, once a user has signed in (checked once a second).
    pub fn take_ready_deferred(&mut self) -> Option<(App, HashMap<String, String>)> {
        if self.deferred.is_none() || self.deferred_check.elapsed() < Duration::from_secs(1) {
            return None;
        }
        self.deferred_check = std::time::Instant::now();
        butterpollo_windows::process::user_signed_in()
            .then(|| self.deferred.take())
            .flatten()
    }
    pub fn exited(&mut self) -> Result<bool> {
        if let Some(tracker) = &mut self.steam {
            use butterpollo_core::steam::Tracked;
            if self.steam_check.elapsed() < Duration::from_secs(1) {
                return Ok(false);
            }
            self.steam_check = std::time::Instant::now();
            let processes = butterpollo_windows::process::processes()?;
            match tracker.update(&processes, butterpollo_windows::process::image_path) {
                Tracked::Waiting | Tracked::Running => return Ok(false),
                Tracked::Exited => {
                    tracing::info!(app = %self.name, "the Steam game exited");
                    return Ok(true);
                }
                Tracked::Unknown => {
                    tracing::info!(app = %self.name, "no game process appeared in the Steam install folder; the stream stays until it is ended");
                    self.steam = None;
                    self.detached = true;
                }
            }
        }
        if self.detached {
            return Ok(false);
        }
        let Some(child) = self.child.as_ref() else {
            return Ok(false);
        };
        let Some(code) = child.exit_code()? else {
            return Ok(false);
        };
        if self.auto_detach && code == 0 && self.started.elapsed() < Duration::from_secs(5) {
            self.detached = true;
            tracing::info!(app=%self.name,"application launcher exited; retaining its streaming session");
            return Ok(false);
        }
        Ok(!self.wait_all || child.active_processes()? == 0)
    }
    /// Return true when the last game transport leaves an app that closes on pause.
    pub fn connection_state(&mut self, connected: bool) -> bool {
        if self.connected == connected {
            return false;
        }
        self.connected = connected;
        if !connected && self.terminate_on_pause {
            return true;
        }
        if let Some(events) = &self.state_events {
            let _ = events.send(connected);
        }
        false
    }
}
pub fn app_bool(app: &App, name: &str, default: bool) -> bool {
    app.extra.get(name).map_or(default, |v| match v {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(s) => matches!(s.as_str(), "true" | "1" | "yes"),
        _ => default,
    })
}
fn expand(value: &str, environment: &BTreeMap<String, String>) -> Result<String> {
    let mut output = String::new();
    let mut rest = value;
    while let Some(index) = rest.find('$') {
        output.push_str(&rest[..index]);
        rest = &rest[index..];
        if let Some(tail) = rest.strip_prefix("$$") {
            output.push('$');
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("$(") {
            let end = tail
                .find(')')
                .ok_or_else(|| anyhow::anyhow!("unterminated environment variable"))?;
            output.push_str(
                environment
                    .get(&tail[..end].to_uppercase())
                    .map_or("", String::as_str),
            );
            rest = &tail[end + 1..];
        } else {
            output.push('$');
            rest = &rest[1..];
        }
    }
    output.push_str(rest);
    Ok(output)
}
/// Whether starting the app runs any command, which needs the user's
/// environment and token.
fn runs_commands(h: &crate::state::Shared, app: &App) -> bool {
    !app.cmd.trim().is_empty()
        || !app.prep.is_empty()
        || app
            .extra
            .get("detached")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|commands| !commands.is_empty())
        || (!app_bool(app, "exclude-global-prep-cmd", false)
            && h.config.read().unwrap().get("global_prep_cmd", "[]").trim() != "[]")
}
pub fn launch(
    h: &crate::state::Shared,
    app: &App,
    args: &HashMap<String, String>,
) -> Result<RunningApp> {
    let mut environment = match butterpollo_windows::process::user_environment() {
        Ok(environment) => environment,
        Err(error) => {
            // Before anyone signs in (a service after a reboot) the sign-in
            // screen can still be streamed. As in Vibepollo, the app's
            // commands wait until a user signs in.
            let mut placeholder = app.clone();
            placeholder.cmd.clear();
            placeholder.prep.clear();
            placeholder.extra.remove("detached");
            placeholder.extra.remove("state-cmd");
            placeholder
                .extra
                .insert("exclude-global-prep-cmd".into(), true.into());
            placeholder
                .extra
                .insert("exclude-global-state-cmd".into(), true.into());
            let mut system: BTreeMap<String, String> = std::env::vars()
                .map(|(key, value)| (key.to_uppercase(), value))
                .collect();
            // The launching client owns the session, as with a started app.
            for (arg, name) in [("clientUuid", "UUID"), ("clientName", "NAME")] {
                if let Some(value) = args.get(arg) {
                    system.insert(format!("SUNSHINE_CLIENT_{name}"), value.clone());
                    system.insert(format!("APOLLO_CLIENT_{name}"), value.clone());
                }
            }
            let mut running = RunningApp::with_environment(&placeholder, system)?;
            if runs_commands(h, app) {
                tracing::info!(app = %app.name, %error, "no signed-in user; the application starts after sign-in");
                running.deferred = Some((app.clone(), args.clone()));
            }
            return Ok(running);
        }
    };
    let mut app = app.clone();
    let document = h.app_document.read().unwrap();
    if let Some(values) = document.get("env").and_then(serde_json::Value::as_object) {
        for (key, value) in values {
            environment.insert(
                key.to_uppercase(),
                expand(
                    value.as_str().ok_or_else(|| {
                        anyhow::anyhow!("application environment must contain strings")
                    })?,
                    &environment,
                )?,
            );
        }
    }
    drop(document);
    if !app_bool(&app, "exclude-global-state-cmd", false) {
        let mut global: Vec<PrepCommand> =
            serde_json::from_str(h.config.read().unwrap().get("global_state_cmd", "[]"))?;
        let mut local: Vec<PrepCommand> = serde_json::from_value(
            app.extra
                .get("state-cmd")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        )?;
        global.append(&mut local);
        app.extra
            .insert("state-cmd".into(), serde_json::to_value(global)?);
    }
    let mode: Vec<_> = args
        .get("mode")
        .map_or("1920x1080x60", String::as_str)
        .split('x')
        .collect();
    let (render_width, render_height) = butterpollo_core::display_policy::render_dimensions(
        mode.first()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1920),
        mode.get(1)
            .and_then(|value| value.parse().ok())
            .unwrap_or(1080),
        args.get("scaleFactor")
            .and_then(|value| value.parse().ok())
            .unwrap_or(100),
        app.extra
            .get("scale-factor")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(100),
    );
    for (name, value) in [
        ("SUNSHINE_APP_ID", app.id().to_string()),
        ("SUNSHINE_APP_NAME", app.name.clone()),
        ("SUNSHINE_CLIENT_WIDTH", render_width.to_string()),
        ("SUNSHINE_CLIENT_HEIGHT", render_height.to_string()),
        (
            "SUNSHINE_CLIENT_FPS",
            mode.get(2).unwrap_or(&"60").to_string(),
        ),
        (
            "SUNSHINE_CLIENT_HDR",
            if args.get("hdrMode").is_some_and(|v| v == "1") {
                "true"
            } else {
                "false"
            }
            .into(),
        ),
        (
            "SUNSHINE_CLIENT_GCMAP",
            args.get("gcmap").cloned().unwrap_or_else(|| "0".into()),
        ),
        (
            "SUNSHINE_CLIENT_HOST_AUDIO",
            if args.get("localAudioPlayMode").is_some_and(|v| v == "1") {
                "true"
            } else {
                "false"
            }
            .into(),
        ),
        (
            "SUNSHINE_CLIENT_ENABLE_SOPS",
            if args.get("sops").is_some_and(|v| v == "1") {
                "true"
            } else {
                "false"
            }
            .into(),
        ),
    ] {
        environment.insert(name.into(), value);
    }
    if let Some(rate) = mode.get(2) {
        let rate = if rate.contains('.') {
            butterpollo_core::framegen::Rate::parse(rate)?
        } else {
            butterpollo_core::framegen::Rate::from_client(rate.parse()?)
        };
        environment.insert(
            "SUNSHINE_CLIENT_FPS".into(),
            if h.config
                .read()
                .unwrap()
                .boolean("envvar_compatibility_mode", false)
            {
                rate.rounded().to_string()
            } else {
                rate.to_string()
            },
        );
    }
    for (key, value) in args {
        let name = match key.as_str() {
            "clientName" => Some("NAME"),
            "clientUuid" => Some("UUID"),
            "surroundParams" => Some("AUDIO_SURROUND_PARAMS"),
            "surroundAudioInfo" => Some("AUDIO_CONFIGURATION"),
            _ => None,
        };
        if let Some(name) = name {
            environment.insert(format!("SUNSHINE_CLIENT_{name}"), value.clone());
        }
    }
    for (key, value) in environment.clone() {
        if let Some(suffix) = key.strip_prefix("SUNSHINE_") {
            environment.insert(format!("APOLLO_{suffix}"), value);
        }
    }
    // Apollo/Vibepollo additions: the app's UUID, the client's own mode
    // before render scaling, and its scale factor.
    for (name, value) in [
        (
            "APOLLO_APP_UUID",
            app.extra
                .get("uuid")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ),
        (
            "APOLLO_CLIENT_RENDER_WIDTH",
            mode.first().unwrap_or(&"1920").to_string(),
        ),
        (
            "APOLLO_CLIENT_RENDER_HEIGHT",
            mode.get(1).unwrap_or(&"1080").to_string(),
        ),
        (
            "APOLLO_CLIENT_SCALE_FACTOR",
            args.get("scaleFactor")
                .cloned()
                .unwrap_or_else(|| "100".into()),
        ),
        ("APOLLO_APP_STATUS", "STARTING".to_owned()),
    ] {
        environment.insert(name.into(), value);
    }
    if !app_bool(&app, "exclude-global-prep-cmd", false) {
        let mut global: Vec<PrepCommand> =
            serde_json::from_str(h.config.read().unwrap().get("global_prep_cmd", "[]"))?;
        global.append(&mut app.prep);
        app.prep = global;
    }
    app.cmd = expand(&app.cmd, &environment)?;
    app.working_dir = expand(&app.working_dir, &environment)?;
    for prep in &mut app.prep {
        prep.r#do = expand(&prep.r#do, &environment)?;
        prep.undo = expand(&prep.undo, &environment)?;
    }
    if let Some(commands) = app
        .extra
        .get_mut("detached")
        .and_then(serde_json::Value::as_array_mut)
    {
        for command in commands {
            *command = serde_json::Value::String(expand(
                command
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("detached command must be a string"))?,
                &environment,
            )?);
        }
    }
    RunningApp::with_environment(&app, environment)
}

impl Drop for RunningApp {
    fn drop(&mut self) {
        self.stop();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn existing_environment_syntax_is_case_insensitive_and_checked() {
        let env = BTreeMap::from([("PATH".into(), "C:\\tools".into())]);
        assert_eq!(
            expand("$(Path);$$;$(MISSING);$literal$", &env).unwrap(),
            "C:\\tools;$;;$literal$"
        );
        assert!(expand("$(Path", &env).is_err());
    }
    #[test]
    fn client_hooks_use_the_saved_environment_on_disconnect() {
        let path = std::env::temp_dir().join(format!(
            "butterpollo-client-hooks-{}.txt",
            uuid::Uuid::new_v4()
        ));
        let environment = BTreeMap::from([("MARKER".into(), path.to_string_lossy().into_owned())]);
        let extra = BTreeMap::from([
            (
                "do".into(),
                serde_json::json!([{"cmd":"echo connected>\"$(MARKER)\""}]),
            ),
            (
                "undo".into(),
                serde_json::json!([{"cmd":"echo disconnected>>\"$(MARKER)\""}]),
            ),
        ]);
        let wait = |needle: &str| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !std::fs::read_to_string(&path)
                .unwrap_or_default()
                .contains(needle)
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "client hook did not write {needle}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let hooks = ClientCommands::with_environment(&extra, environment.clone()).unwrap();
        wait("connected");
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("disconnected")
        );
        drop(hooks);
        wait("disconnected");
        assert_eq!(
            std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            ["connected", "disconnected"]
        );
        std::fs::remove_file(&path).unwrap();
        let invalid = BTreeMap::from([
            ("do".into(), extra["do"].clone()),
            ("undo".into(), serde_json::json!([{"cmd":null}])),
        ]);
        assert!(ClientCommands::with_environment(&invalid, environment).is_err());
        assert!(!path.exists());
    }
}
