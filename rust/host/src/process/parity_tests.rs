use super::*;
use crate::state::test_support::Fixture;
use serde_json::json;

#[test]
fn before_sign_in_app_commands_are_deferred_and_the_requesting_device_keeps_ownership() {
    let f = Fixture::new();
    let app: App = serde_json::from_value(json!({"name":"Deferred","cmd":"must not execute",
        "prep-cmd":[{"do":"must not prepare","undo":"must not undo"}],
        "detached":["must not detach"],"state-cmd":[{"do":"must not run","undo":""}]}))
    .unwrap();
    let args = HashMap::from([
        ("clientUuid".into(), "owner".into()),
        ("clientName".into(), "Phone".into()),
    ]);
    let running = launch_with_environment(
        &f.host,
        &app,
        &args,
        Err(anyhow::anyhow!("no signed-in user")),
    )
    .unwrap();
    assert!(running.child.is_none());
    assert!(running.undo.is_empty());
    assert!(running.state_events.is_none());
    assert_eq!(running.owner, "owner");
    assert_eq!(running.environment["APOLLO_CLIENT_UUID"], "owner");
    assert_eq!(running.environment["APOLLO_CLIENT_NAME"], "Phone");
    let (deferred, options) = running.deferred.as_ref().unwrap();
    assert_eq!(
        serde_json::to_value(deferred).unwrap(),
        serde_json::to_value(&app).unwrap()
    );
    assert_eq!(options, &args);
}

#[test]
fn app_environment_expands_apollo_variables_and_preserves_fractional_rates_and_saved_apps() {
    let f = Fixture::new();
    let app: App =
        serde_json::from_value(json!({"name":"Game","uuid":"game-id","scale-factor":150})).unwrap();
    let original = serde_json::to_value(&app).unwrap();
    *f.host.app_document.write().unwrap() = json!({"env":{"GAME_DATA":"$(base)/data"},"apps":[]});
    let args = HashMap::from([
        ("mode".into(), "1920x1080x59940".into()),
        ("clientName".into(), "Phone".into()),
        ("clientUuid".into(), "phone-id".into()),
        ("scaleFactor".into(), "200".into()),
        ("hdrMode".into(), "1".into()),
        ("surroundAudioInfo".into(), "6".into()),
        ("localAudioPlayMode".into(), "1".into()),
    ]);
    let running = launch_with_environment(
        &f.host,
        &app,
        &args,
        Ok(BTreeMap::from([("BASE".into(), "fixture".into())])),
    )
    .unwrap();
    for (key, expected) in [
        ("GAME_DATA", "fixture/data"),
        ("APOLLO_APP_UUID", "game-id"),
        ("APOLLO_APP_NAME", "Game"),
        ("APOLLO_CLIENT_NAME", "Phone"),
        ("APOLLO_CLIENT_UUID", "phone-id"),
        ("APOLLO_CLIENT_WIDTH", "2880"),
        ("APOLLO_CLIENT_HEIGHT", "1620"),
        ("APOLLO_CLIENT_RENDER_WIDTH", "1920"),
        ("APOLLO_CLIENT_RENDER_HEIGHT", "1080"),
        ("APOLLO_CLIENT_SCALE_FACTOR", "200"),
        ("APOLLO_CLIENT_FPS", "59.940"),
        ("APOLLO_CLIENT_HDR", "true"),
        ("APOLLO_CLIENT_HOST_AUDIO", "true"),
        ("APOLLO_CLIENT_AUDIO_CONFIGURATION", "6"),
        ("APOLLO_APP_STATUS", "STARTING"),
    ] {
        assert_eq!(running.environment[key], expected, "{key}");
    }
    assert_eq!(running.owner, "phone-id");
    assert_eq!(serde_json::to_value(&app).unwrap(), original);
    f.host
        .config
        .write()
        .unwrap()
        .values
        .insert("envvar_compatibility_mode".into(), "true".into());
    let running = launch_with_environment(&f.host, &app, &args, Ok(BTreeMap::new())).unwrap();
    assert_eq!(running.environment["APOLLO_CLIENT_FPS"], "60");
}

#[test]
fn preparation_undo_and_failed_preparation_keep_global_order_and_rollback() {
    let f = Fixture::new();
    let trace = f.host.directory.join("commands.txt");
    let command = |name: &str| format!("echo {name}>>\"{}\"", trace.display());
    f.host.config.write().unwrap().values.insert(
        "global_prep_cmd".into(),
        json!([
            {"do":command("global"),"undo":command("undo-global")}
        ])
        .to_string(),
    );
    let mut app: App = serde_json::from_value(json!({"name":"Commands","prep-cmd":[
        {"do":command("app"),"undo":command("undo-app")}
    ]}))
    .unwrap();
    let running =
        launch_with_environment(&f.host, &app, &HashMap::new(), Ok(BTreeMap::new())).unwrap();
    assert_eq!(
        std::fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["global", "app"]
    );
    drop(running);
    assert_eq!(
        std::fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["global", "app", "undo-app", "undo-global"]
    );
    std::fs::write(&trace, "").unwrap();
    app.prep.push(PrepCommand {
        r#do: "exit /b 7".into(),
        undo: command("must-not-run"),
        elevated: false,
    });
    let error = launch_with_environment(&f.host, &app, &HashMap::new(), Ok(BTreeMap::new()))
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "preparation command exited with 7");
    assert_eq!(
        std::fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["global", "app", "undo-app", "undo-global"]
    );
}

#[test]
fn working_directory_inference_uses_only_absolute_programs_with_existing_parents() {
    let f = Fixture::new();
    let folder = f.host.directory.join("A game");
    std::fs::create_dir(&folder).unwrap();
    let command = format!("\"{}\" --windowed", folder.join("game.exe").display());
    assert_eq!(inferred_working_dir(&command), folder.to_string_lossy());
    for command in ["game.exe --windowed", "steam://rungameid/570", ""] {
        assert_eq!(inferred_working_dir(command), "");
    }
    assert_eq!(
        inferred_working_dir(&format!(
            "\"{}\"",
            folder.join("missing/game.exe").display()
        )),
        ""
    );
}

#[test]
fn detached_app_commands_run_with_the_supplied_environment() {
    let f = Fixture::new();
    let output = f.host.directory.join("detached.txt");
    let app: App = serde_json::from_value(json!({"name":"Detached","detached":[
        format!("echo $(APOLLO_APP_NAME)>\"{}\"", output.display())
    ]}))
    .unwrap();
    let running =
        launch_with_environment(&f.host, &app, &HashMap::new(), Ok(BTreeMap::new())).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_to_string(&output).is_ok_and(|text| text.trim() == "Detached") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "detached command did not finish"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(running);
}
