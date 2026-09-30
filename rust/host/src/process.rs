use anyhow::{Result, bail};
use butterpollo_core::state::{App, PrepCommand};
use butterpollo_windows::process::Process;
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    time::Duration,
};
pub struct RunningApp {
    pub id: u32,
    pub name: String,
    pub child: Option<Process>,
    undo: Vec<PrepCommand>,
    working: String,
    environment: BTreeMap<String, String>,
    started: std::time::Instant,
    auto_detach: bool,
    wait_all: bool,
    detached: bool,
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
            child: None,
            undo: vec![],
            working: app.working_dir.clone(),
            environment,
            started: std::time::Instant::now(),
            auto_detach: app_bool(app, "auto-detach", true),
            wait_all: app_bool(app, "wait-all", true),
            detached: false,
        };
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
        if let Some(child) = self.child.take() {
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
    pub fn exited(&mut self) -> Result<bool> {
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
pub fn launch(
    h: &crate::state::Shared,
    app: &App,
    args: &HashMap<String, String>,
) -> Result<RunningApp> {
    let mut app = app.clone();
    let mut environment = butterpollo_windows::process::user_environment()?;
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
    let mode: Vec<_> = args
        .get("mode")
        .map_or("1920x1080x60", String::as_str)
        .split('x')
        .collect();
    for (name, value) in [
        ("SUNSHINE_APP_ID", app.id().to_string()),
        ("SUNSHINE_APP_NAME", app.name.clone()),
        (
            "SUNSHINE_CLIENT_WIDTH",
            mode.first().unwrap_or(&"1920").to_string(),
        ),
        (
            "SUNSHINE_CLIENT_HEIGHT",
            mode.get(1).unwrap_or(&"1080").to_string(),
        ),
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
}
