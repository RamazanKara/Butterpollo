use crate::state::Shared;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn trigger_update(h: &Shared) {
    let mut state = h.updates.lock().unwrap();
    if state["checking"] == true {
        return;
    }
    state["checking"] = json!(true);
    drop(state);
    let h = h.clone();
    tokio::spawn(async move {
        let result = async {
            let client = reqwest::Client::builder()
                .https_only(true)
                .timeout(Duration::from_secs(35))
                .user_agent("Butterpollo-Rust")
                .build()?;
            let mut response = client
                .get("https://api.github.com/repos/RamazanKara/Butterpollo/releases?per_page=30")
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
                            && r["assets"].as_array().is_some_and(|assets| {
                                assets.iter().any(|a| {
                                    a["name"].as_str().is_some_and(|s| {
                                        s.starts_with("butterpollo-rust-") && s.ends_with(".zip")
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
            Ok(releases) => state["releases"] = json!(releases),
            Err(error) => tracing::warn!(%error,"release check failed"),
        }
    });
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
    let path = h.directory.join("display-baseline-rust.json");
    let snapshot = if path.exists() {
        Some(serde_json::from_slice::<
            butterpollo_windows::display::Snapshot,
        >(&std::fs::read(path)?)?)
    } else {
        None
    };
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
    Ok(json!({"status":true}))
}
pub fn restore_golden(h: &Shared) -> Result<Value> {
    if h.sessions.lock().unwrap().owns_capture() || !h.monitors.lock().unwrap().is_empty() {
        bail!("disconnect all streams and remote monitors before restoring a baseline");
    }
    let snapshot: butterpollo_windows::display::Snapshot = serde_json::from_slice(&std::fs::read(
        h.directory.join("display-baseline-rust.json"),
    )?)?;
    snapshot.restore()?;
    Ok(json!({"status":true}))
}
pub fn bundle_manifest(h: &Shared) -> Value {
    let dump = newest_dump(h).map_or(0, |d| d.size);
    json!({"status":true,"parts":[{"index":1,"filename":"butterpollo-support.zip","estimated_size_bytes":dump+8*1024*1024}]})
}
pub fn bundle(h: &Shared) -> Result<PathBuf> {
    use zip::{ZipWriter, write::SimpleFileOptions};
    let directory = h.directory.join("support");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.zip", uuid::Uuid::new_v4()));
    let mut writer = ZipWriter::new(std::fs::File::create(&path)?);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for relative in ["logs/butterpollo.log", "crashes/panic.txt"] {
        if let Ok(mut file) = std::fs::File::open(h.directory.join(relative)) {
            let length = file.metadata()?.len();
            file.seek(SeekFrom::Start(length.saturating_sub(8 * 1024 * 1024)))?;
            writer.start_file(relative, options)?;
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
    let c = h.config.read().unwrap();
    let configured = c.get("rtss_path", "");
    let root = if configured.is_empty() {
        Path::new("C:/Program Files (x86)/RivaTuner Statistics Server")
    } else {
        Path::new(configured)
    };
    let path = root.join("RTSS.exe");
    let exists = path.exists();
    let hooks = root.join("RTSSHooks64.dll").exists();
    json!({"status":true,"enabled":c.boolean("frame_limiter_enable",false),"configured_provider":c.get("frame_limiter_provider","auto"),"active_provider":"none","nvidia_available":false,"nvcp_ready":false,"rtss_available":exists&&hooks,"disable_vsync":false,"nv_overrides_supported":false,"configured_path":configured,"path_configured":!configured.is_empty(),"resolved_path":root,"path_exists":exists,"hooks_found":hooks,"profile_found":root.join("Profiles/Global").exists(),"can_bootstrap_profile":exists,"process_running":false,"message":"Automatic external frame-limiter overrides are not available in this Rust build."})
}
