use crate::state::Shared;
use anyhow::{Context, Result, bail};
use butterpollo_core::version::newer;
use serde_json::{Value, json};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Look for a newer release. `tell` (the tray's "Check for updates")
/// also reports the result in a notification, even an update already
/// announced.
pub fn trigger_update(h: &Shared, tell: bool) {
    let mut state = h.updates.lock().unwrap();
    if state["checking"] == true {
        return;
    }
    state["checking"] = json!(true);
    drop(state);
    let h = h.clone();
    tokio::spawn(async move {
        let result = async {
            let client = crate::updater::client(Duration::from_secs(35))?;
            let mut response = client
                .get(crate::updater::RELEASES)
                .send()
                .await?
                .error_for_status()?;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                    bail!("release response exceeds the limit");
                }
                bytes.extend_from_slice(&chunk);
            }
            let releases: Vec<Value> = serde_json::from_slice(&bytes)?;
            // This executable must never advertise an installer for the former C++ host.
            Ok::<_, anyhow::Error>(
                releases
                    .into_iter()
                    .filter(|r| {
                        r["draft"] != true
                            && (r["prerelease"] != true
                                || h.config
                                    .read()
                                    .unwrap()
                                    .boolean("notify_pre_releases", false))
                            && r["assets"].as_array().is_some_and(|assets| {
                                assets.iter().any(|a| {
                                    a["name"].as_str().is_some_and(|s| {
                                        (s.starts_with("butterpollo-rust-") && s.ends_with(".zip"))
                                            || (s.starts_with("butterpollo-setup-")
                                                && s.ends_with(".exe"))
                                    })
                                })
                            })
                    })
                    .collect::<Vec<_>>(),
            )
        }
        .await;
        let mut state = h.updates.lock().unwrap();
        state["checking"] = json!(false);
        state["checked_at"] = json!(now());
        state["check_failed"] = json!(result.is_err());
        match result {
            Ok(releases) => {
                let current = env!("CARGO_PKG_VERSION");
                let latest = releases
                    .iter()
                    .filter_map(|r| r["tag_name"].as_str())
                    .filter(|tag| newer(tag, current))
                    .max_by(|a, b| {
                        if newer(a, b) {
                            std::cmp::Ordering::Greater
                        } else if newer(b, a) {
                            std::cmp::Ordering::Less
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    })
                    .map(str::to_owned);
                state["current_version"] = json!(current);
                state["update_available"] = json!(latest.is_some());
                state["latest_version"] = json!(latest);
                state["releases"] = json!(releases);
                state["check_error"] = Value::Null;
                drop(state);
                if tell && latest.is_none() {
                    butterpollo_windows::tray::notify(
                        "Butterpollo is up to date",
                        &format!("Version {current} is the latest."),
                    );
                }
                if let Some(latest) = latest {
                    if !announce(&h, &latest) && tell {
                        notify_update(&latest);
                    }
                    let automatic = h.config.read().unwrap().boolean("auto_update", false);
                    if automatic && let Err(error) = crate::updater::queue(&h, true) {
                        tracing::warn!(%error, "automatic update could not be queued");
                    }
                }
            }
            Err(error) => {
                state["check_error"] = json!(error.to_string());
                tracing::warn!(%error,"release check failed");
                if tell {
                    butterpollo_windows::tray::notify(
                        "Update check failed",
                        "GitHub could not be reached. See Maintenance in the console.",
                    );
                }
            }
        }
    });
}
/// Tell the user about a new version once, as Vibepollo does; false when
/// it was told before.
fn announce(h: &Shared, version: &str) -> bool {
    let mut aliases = h.aliases.lock().unwrap();
    if aliases["root"]["last_notified_version"].as_str() == Some(version) {
        return false;
    }
    let mut next = aliases.clone();
    if !next["root"].is_object() {
        next["root"] = json!({});
    }
    next["root"]["last_notified_version"] = json!(version);
    if butterpollo_core::state::write_json(&h.aliases_path, &next).is_ok() {
        *aliases = next;
    }
    drop(aliases);
    tracing::info!(version, "a newer Butterpollo is available");
    notify_update(version);
    true
}
fn notify_update(version: &str) {
    butterpollo_windows::tray::notify(
        "Butterpollo update",
        &format!(
            "Version {} is available. See Maintenance in the console.",
            version.trim_start_matches('v')
        ),
    );
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn iso(time: SystemTime) -> String {
    time::OffsetDateTime::from(time)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}
struct Dump {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}
fn newest_dump(h: &Shared) -> Option<Dump> {
    let mut roots = vec![h.directory.join("crashes")];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join("CrashDumps"));
    }
    roots
        .into_iter()
        .flat_map(|root| {
            std::fs::read_dir(root)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
        })
        .take(4096)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if !(name.starts_with("butterpollo.") || name.starts_with("sunshine."))
                || !name.ends_with(".dmp")
            {
                return None;
            }
            let metadata = e.metadata().ok()?;
            let modified = metadata.modified().ok()?;
            if !metadata.is_file()
                || metadata.len() > 512 * 1024 * 1024
                || SystemTime::now().duration_since(modified).ok()?.as_secs() > 7 * 86400
            {
                return None;
            }
            Some(Dump {
                path: e.path(),
                size: metadata.len(),
                modified,
            })
        })
        .max_by_key(|d| d.modified)
}
pub fn crash_status(h: &Shared) -> Result<Value> {
    let Some(dump) = newest_dump(h) else {
        return Ok(json!({"available":false,"dismissed":false}));
    };
    let filename = dump.path.file_name().unwrap().to_string_lossy();
    let captured = iso(dump.modified);
    let dismissed =
        butterpollo_core::state::load_json(&h.directory.join("crash-dismissal.json"), json!({}))?;
    Ok(
        json!({"available":true,"filename":filename,"path":dump.path,"process":filename.split('.').next(),"size_bytes":dump.size,"captured_at":captured,"age_seconds":SystemTime::now().duration_since(dump.modified)?.as_secs(),"dismissed":dismissed["filename"].as_str()==Some(filename.as_ref()) && dismissed["captured_at"]==captured,"dismissed_at":dismissed["dismissed_at"]}),
    )
}
pub fn dismiss_crash(h: &Shared, data: &Value) -> Result<Value> {
    let status = crash_status(h)?;
    if status["available"] != true
        || status["filename"] != data["filename"]
        || status["captured_at"] != data["captured_at"]
    {
        bail!("crash dump metadata changed");
    }
    let dismissed_at = iso(SystemTime::now());
    butterpollo_core::state::write_json(
        &h.directory.join("crash-dismissal.json"),
        &json!({"filename":status["filename"],"captured_at":status["captured_at"],"dismissed_at":dismissed_at}),
    )?;
    Ok(json!({"status":true,"dismissed_at":dismissed_at}))
}
pub fn golden_status(h: &Shared, compare: bool) -> Result<Value> {
    let snapshot = baseline(h)?;
    let mismatch = if compare && let Some(old) = snapshot.as_ref() {
        let current = butterpollo_windows::display::Snapshot::capture()?;
        serde_json::to_value(&current)? != serde_json::to_value(old)?
    } else {
        false
    };
    Ok(
        json!({"status":true,"exists":snapshot.is_some(),"snapshot_version":snapshot.as_ref().map(|s|s.version),"latest_snapshot_version":1,"has_layout":snapshot.is_some(),"needs_layout_upgrade":false,"out_of_date":false,"comparison_available":compare,"current_mismatch_reason":if mismatch {"display_configuration_changed"} else {""},"restore_failure_count":0,"restore_status_reason":""}),
    )
}
pub fn capture_golden(h: &Shared) -> Result<Value> {
    if h.sessions.lock().unwrap().owns_capture() || !h.monitors.lock().unwrap().is_empty() {
        bail!("disconnect all streams and remote monitors before capturing a baseline");
    }
    butterpollo_core::state::write_json(
        &h.directory.join("display-baseline-rust.json"),
        &butterpollo_windows::display::Snapshot::capture()?,
    )?;
    let tombstone = h.directory.join("display-baseline-disabled");
    if tombstone.exists() {
        std::fs::remove_file(tombstone)?;
    }
    Ok(json!({"status":true}))
}
pub fn restore_golden(h: &Shared) -> Result<Value> {
    if h.sessions.lock().unwrap().owns_capture() || !h.monitors.lock().unwrap().is_empty() {
        bail!("disconnect all streams and remote monitors before restoring a baseline");
    }
    let snapshot = baseline(h)?.context("saved display baseline is unavailable")?;
    snapshot.restore_excluding(&display_exclusions(&h.config.read().unwrap())?)?;
    Ok(json!({"status":true}))
}
/// Displays a baseline restore leaves alone. Vibepollo also accepts
/// `{"devices": [...]}` and entries naming `device_id` or `id`.
pub fn display_exclusions(config: &butterpollo_core::config::Config) -> Result<Vec<String>> {
    let value = config.get("dd_snapshot_exclude_devices", "");
    let parsed = serde_json::from_str::<serde_json::Value>(value).ok();
    let entries = parsed.as_ref().and_then(|v| {
        v.as_array().or_else(|| {
            v.get("exclude_devices")
                .or_else(|| v.get("devices"))
                .and_then(serde_json::Value::as_array)
        })
    });
    let Some(entries) = entries else {
        return Ok(config.list("dd_snapshot_exclude_devices"));
    };
    Ok(entries
        .iter()
        .filter_map(|entry| {
            entry
                .as_str()
                .or_else(|| entry.get("device_id").and_then(serde_json::Value::as_str))
                .or_else(|| entry.get("id").and_then(serde_json::Value::as_str))
        })
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .collect())
}
/// Read previous snapshots without modifying the old installation's files.
pub fn baseline(h: &Shared) -> Result<Option<butterpollo_windows::display::Snapshot>> {
    let own = h.directory.join("display-baseline-rust.json");
    if own.exists() {
        return Ok(Some(butterpollo_windows::display::Snapshot::read(&own)?));
    }
    if h.directory.join("display-baseline-disabled").exists() {
        return Ok(None);
    }
    let mut candidates = vec![h.directory.join("display_golden_restore.json")];
    // Before sign-in there is no user whose folders could hold an old
    // snapshot; only the machine-wide folder is searched then (issue #6).
    let environment = butterpollo_windows::process::user_environment().unwrap_or_else(|error| {
        tracing::debug!(%error, "no signed-in user; searching only machine-wide display snapshots");
        std::env::var("PROGRAMDATA")
            .map(|root| [("PROGRAMDATA".to_owned(), root)].into())
            .unwrap_or_default()
    });
    for key in ["APPDATA", "LOCALAPPDATA", "PROGRAMDATA"] {
        if let Some(root) = environment.get(key) {
            candidates.push(PathBuf::from(root).join("Sunshine/display_golden_restore.json"));
        }
    }
    for path in candidates {
        if path.is_file() {
            return Ok(Some(butterpollo_windows::display::Snapshot::read(&path)?));
        }
    }
    Ok(None)
}
pub fn bundle_manifest(h: &Shared) -> Value {
    let dump = newest_dump(h).map_or(0, |d| d.size);
    json!({"status":true,"parts":[{"index":1,"filename":"butterpollo-support.zip","estimated_size_bytes":dump+8*1024*1024}]})
}
/// The host log, honouring `log_path`; the default file when the host could
/// not use it and logs there instead.
pub fn log_path(h: &Shared) -> PathBuf {
    let configured =
        h.config
            .read()
            .unwrap()
            .path("log_path", &h.directory, "logs/butterpollo.log");
    if configured.is_file() {
        configured
    } else {
        h.directory.join("logs/butterpollo.log")
    }
}
pub fn bundle(h: &Shared) -> Result<PathBuf> {
    use zip::{ZipWriter, write::SimpleFileOptions};
    let directory = h.directory.join("support");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.zip", uuid::Uuid::new_v4()));
    let mut writer = ZipWriter::new(std::fs::File::create(&path)?);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let host_log = log_path(h);
    let service_log = h.directory.join("logs/service.log");
    for (name, path) in [
        ("logs/butterpollo.log", host_log.clone()),
        (
            "logs/butterpollo.log.1",
            butterpollo_core::logfile::RotatingFile::rotated(&host_log, 1),
        ),
        ("logs/service.log", service_log),
        ("crashes/panic.txt", h.directory.join("crashes/panic.txt")),
    ] {
        if let Ok(mut file) = std::fs::File::open(path) {
            let length = file.metadata()?.len();
            file.seek(SeekFrom::Start(length.saturating_sub(8 * 1024 * 1024)))?;
            writer.start_file(name, options)?;
            std::io::copy(&mut file.take(8 * 1024 * 1024), &mut writer)?;
        }
    }
    writer.start_file("diagnostics.json", options)?;
    let config = h.config.read().unwrap();
    let values: serde_json::Map<_, _> = config
        .values
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                if ["password", "token", "key", "secret", "command", "cmd"]
                    .iter()
                    .any(|s| k.contains(s))
                {
                    json!("[redacted]")
                } else {
                    json!(v)
                },
            )
        })
        .collect();
    writer.write_all(&serde_json::to_vec_pretty(&json!({"version":env!("CARGO_PKG_VERSION"),"platform":"windows","config":values,"codecs":h.codecs.load(std::sync::atomic::Ordering::Acquire)}))?)?;
    drop(config);
    if let Some(dump) = newest_dump(h) {
        writer.start_file(
            format!(
                "crashes/{}",
                dump.path
                    .file_name()
                    .context("dump filename missing")?
                    .to_string_lossy()
            ),
            options,
        )?;
        std::io::copy(
            &mut std::fs::File::open(dump.path)?.take(512 * 1024 * 1024),
            &mut writer,
        )?;
    }
    writer.finish()?.sync_all()?;
    Ok(path)
}
pub fn integration_status(h: &Shared) -> Value {
    butterpollo_windows::limiter::status(&h.config.read().unwrap())
}
