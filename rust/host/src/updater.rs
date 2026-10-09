//! Only release metadata from our fixed GitHub repository can select an installer.
//! Installation is limited to the installed service and serialized with launches.
use crate::state::Shared;
use anyhow::{Context, Result, bail};
use butterpollo_core::version::newer;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncWriteExt;

/// The repository was RamazanKara/Butterpollo until the product became
/// Rubylight. Before the rename only the old name answers; after it, the old
/// name redirects to an API address `client` refuses, so the new one is first.
const REPOSITORIES: [&str; 2] = ["RamazanKara/Rubylight", "RamazanKara/Butterpollo"];
/// Installer names, current first. Releases also carry the old name so that
/// hosts from before the rename find them.
const INSTALLERS: [&str; 2] = ["rubylight-setup", "butterpollo-setup"];
const MAX_INSTALLER: u64 = 512 * 1024 * 1024;
const IDLE_SECONDS: u64 = 60;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Debug)]
pub struct Installer {
    pub version: String,
    pub url: String,
    pub digest: String,
    pub size: u64,
}

/// The release list from the first repository name GitHub knows.
pub async fn releases(client: &reqwest::Client) -> Result<reqwest::Response> {
    let mut repositories = REPOSITORIES.iter().peekable();
    while let Some(repository) = repositories.next() {
        let response = client
            .get(format!(
                "https://api.github.com/repos/{repository}/releases?per_page=30"
            ))
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND && repositories.peek().is_some() {
            continue;
        }
        return Ok(response.error_for_status()?);
    }
    unreachable!("REPOSITORIES is not empty")
}

pub fn installer(release: &Value) -> Result<Installer> {
    let tag = release["tag_name"]
        .as_str()
        .context("release has no version")?;
    let version = tag.trim_start_matches('v');
    if version.is_empty()
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
    {
        bail!("invalid release version");
    }
    let assets = release["assets"]
        .as_array()
        .context("release has no assets")?;
    let (name, asset) = INSTALLERS
        .iter()
        .find_map(|prefix| {
            let name = format!("{prefix}-{version}.exe");
            let asset = assets.iter().find(|asset| asset["name"] == name)?;
            Some((name, asset))
        })
        .context("release has no Windows installer")?;
    let url = REPOSITORIES
        .iter()
        .map(|repository| format!("https://github.com/{repository}/releases/download/{tag}/{name}"))
        .find(|url| asset["browser_download_url"] == *url)
        .context("installer is not from the Rubylight release repository")?;
    let digest = asset["digest"]
        .as_str()
        .and_then(|s| s.strip_prefix("sha256:"))
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .context("GitHub has not supplied a SHA-256 digest for this installer")?;
    let size = asset["size"]
        .as_u64()
        .filter(|s| *s > 0 && *s <= MAX_INSTALLER)
        .context("installer size is invalid")?;
    Ok(Installer {
        version: tag.into(),
        url,
        digest: digest.to_ascii_lowercase(),
        size,
    })
}

pub fn client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .timeout(timeout)
        .user_agent("Rubylight")
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            if attempt.previous().len() < 5
                && url.scheme() == "https"
                && matches!(
                    url.host_str(),
                    Some(
                        "github.com"
                            | "release-assets.githubusercontent.com"
                            | "objects.githubusercontent.com"
                    )
                )
            {
                attempt.follow()
            } else {
                attempt.error("unexpected release download redirect")
            }
        }))
        .build()?)
}

fn supported(h: &Shared) -> bool {
    let profile = butterpollo_core::paths::installed_profile();
    butterpollo_windows::process::is_system()
        && std::fs::canonicalize(&h.directory)
            .ok()
            .zip(std::fs::canonicalize(profile).ok())
            .is_some_and(|(a, b)| a == b)
}

pub fn status(h: &Shared) -> Value {
    let mut state = h.updates.lock().unwrap().clone();
    state["install_supported"] = json!(supported(h));
    state["auto_update"] = json!(h.config.read().unwrap().boolean("auto_update", false));
    state["last_install"] =
        butterpollo_core::state::load_json(&h.directory.join("update-result.json"), json!(null))
            .unwrap_or(Value::Null);
    state
}

pub fn installing(h: &Shared) -> bool {
    h.updates.lock().unwrap()["phase"] == "installing"
}

pub fn busy(h: &Shared) -> bool {
    let sessions = h.sessions.lock().unwrap();
    let busy = !sessions.active.is_empty() || !sessions.pending.is_empty();
    drop(sessions);
    busy || h.current_app.lock().unwrap().is_some() || !h.monitors.lock().unwrap().is_empty()
}

pub fn queue(h: &Shared, automatic: bool) -> Result<()> {
    if !supported(h) {
        bail!("Install Rubylight as a Windows service to use in-app updates");
    }
    let config = h.config.read().unwrap();
    let prereleases = config.boolean("notify_pre_releases", false);
    drop(config);
    let mut state = h.updates.lock().unwrap();
    if matches!(
        state["phase"].as_str(),
        Some("waiting" | "downloading" | "ready" | "installing")
    ) {
        return Ok(());
    }
    if state["check_failed"] == true {
        bail!("Check for updates successfully before installing");
    }
    let version = state["latest_version"]
        .as_str()
        .context("No newer release is available")?;
    let release = state["releases"]
        .as_array()
        .and_then(|r| r.iter().find(|r| r["tag_name"] == version))
        .context("Check for updates again")?;
    if release["prerelease"] == true && !prereleases {
        bail!("Pre-release updates are disabled");
    }
    if !newer(version, env!("CARGO_PKG_VERSION")) {
        bail!("This version is already installed");
    }
    let candidate = installer(release)?;
    let last = check_recovery(&h.directory)?;
    if automatic {
        // Never loop on a bad release after a rollback or interrupted installer.
        if last["version"]
            .as_str()
            .is_some_and(|v| v.trim_start_matches('v') == candidate.version.trim_start_matches('v'))
        {
            return Ok(());
        }
        if state["attempted_version"] == candidate.version {
            return Ok(());
        }
    }
    state["phase"] = json!("waiting");
    state["queued_version"] = json!(candidate.version);
    state["automatic"] = json!(automatic);
    state["idle_since"] = json!(0);
    state["error"] = Value::Null;
    Ok(())
}

pub fn cancel(h: &Shared) -> Result<()> {
    let mut state = h.updates.lock().unwrap();
    if state["phase"] == "installing" {
        bail!("Installation has already started");
    }
    state["phase"] = json!("idle");
    // A generation token prevents a cancelled download from publishing its result.
    state["download_id"] = Value::Null;
    state["queued_version"] = Value::Null;
    state["attempted_version"] = state["latest_version"].clone();
    Ok(())
}

/// Called by maintenance. All admission checks and the final handoff share the
/// Moonlight launch lock, so a connection cannot slip between the check and setup.
pub fn poll(h: &Shared) {
    let Ok(_transition) = h.launch_transition.try_lock() else {
        return;
    };
    let phase = h.updates.lock().unwrap()["phase"]
        .as_str()
        .unwrap_or("idle")
        .to_owned();
    if !matches!(phase.as_str(), "waiting" | "ready" | "downloading") {
        return;
    }
    let config = h.config.read().unwrap();
    let auto_enabled =
        config.boolean("auto_update", false) && config.integer("update_check_interval", 86400) > 0;
    let prereleases = config.boolean("notify_pre_releases", false);
    drop(config);
    let mut state = h.updates.lock().unwrap();
    if state["phase"] != phase {
        return;
    }
    let release = state["releases"]
        .as_array()
        .and_then(|r| r.iter().find(|r| r["tag_name"] == state["queued_version"]))
        .cloned();
    if (state["automatic"] == true && !auto_enabled)
        || release
            .as_ref()
            .is_some_and(|r| r["prerelease"] == true && !prereleases)
    {
        state["phase"] = json!("idle");
        state["download_id"] = Value::Null;
        return;
    }
    drop(state);
    let is_busy = busy(h);
    let mut state = h.updates.lock().unwrap();
    if state["phase"] != phase {
        return;
    }
    if is_busy {
        state["idle_since"] = json!(0);
        return;
    }
    let idle_since = state["idle_since"].as_u64().unwrap_or(0);
    if idle_since == 0 {
        state["idle_since"] = json!(now());
        return;
    }
    if now().saturating_sub(idle_since) < IDLE_SECONDS || phase == "downloading" {
        return;
    }
    if phase == "waiting" {
        let candidate = release
            .as_ref()
            .context("release metadata is unavailable")
            .and_then(installer);
        let candidate = match candidate {
            Ok(c) => c,
            Err(error) => {
                failed(&mut state, error);
                return;
            }
        };
        let id = uuid::Uuid::new_v4().to_string();
        state["phase"] = json!("downloading");
        state["download_id"] = json!(id);
        state["downloaded_bytes"] = json!(0);
        state["download_size"] = json!(candidate.size);
        state["attempted_version"] = json!(candidate.version);
        drop(state);
        let h = h.clone();
        tokio::spawn(async move {
            let result = download(&h, &id, &candidate).await;
            let mut state = h.updates.lock().unwrap();
            if state["download_id"] != id {
                drop(state);
                if let Ok(path) = result {
                    let _ = std::fs::remove_dir_all(path.parent().unwrap());
                }
                return;
            }
            match result {
                Ok(path) => {
                    state["installer_path"] = json!(path);
                    state["phase"] = json!("ready");
                }
                Err(error) if error.is::<DownloadPaused>() => {
                    state["phase"] = json!("waiting");
                    state["idle_since"] = json!(0);
                    state["downloaded_bytes"] = json!(0);
                }
                Err(error) => failed(&mut state, error),
            }
        });
    } else {
        let result = (|| -> Result<()> {
            if !supported(h) {
                bail!("Updates require the installed Windows service");
            }
            let path = PathBuf::from(
                state["installer_path"]
                    .as_str()
                    .context("installer missing")?,
            );
            let candidate = installer(release.as_ref().context("release missing")?)?;
            let mut file = std::fs::File::open(&path)?;
            let mut hash = Sha256::new();
            let size = std::io::copy(&mut file, &mut hash)?;
            verify(size, &hex::encode(hash.finalize()), &candidate)?;
            let install = std::env::current_exe()?
                .parent()
                .context("installation folder missing")?
                .to_path_buf();
            // A persistent attempt record also prevents automatic retry loops.
            check_recovery(&h.directory)?;
            butterpollo_core::state::write_json(
                &h.directory.join("update-result.json"),
                &json!({"version":candidate.version,"phase":"installing","started_at":now()}),
            )?;
            let mut command = std::process::Command::new(path);
            use std::os::windows::process::CommandExt;
            // The installer must survive the service stopping its host job.
            command
                .creation_flags(0x01000000 | 0x08000000)
                .args(["--quiet", "--update", "--install-dir"])
                .arg(install)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let mut child = command.spawn().context("starting the update installer")?;
            let h = h.clone();
            std::thread::spawn(move || {
                let result = child.wait();
                // If the old host is still here, setup failed before the restart.
                let mut state = h.updates.lock().unwrap();
                failed(
                    &mut state,
                    anyhow::anyhow!(
                        "Installer exited before replacing this host: {result:?}. See the setup log."
                    ),
                );
            });
            Ok(())
        })();
        match result {
            Ok(()) => state["phase"] = json!("installing"),
            Err(error) => failed(&mut state, error),
        }
    }
}

fn failed(state: &mut Value, error: anyhow::Error) {
    tracing::warn!(%error, "update failed");
    state["phase"] = json!("failed");
    state["error"] = json!(format!("{error:#}"));
}

fn verify(size: u64, digest: &str, candidate: &Installer) -> Result<()> {
    if size != candidate.size || digest != candidate.digest {
        bail!("Installer verification failed; no files were installed");
    }
    Ok(())
}

async fn download(h: &Shared, id: &str, candidate: &Installer) -> Result<PathBuf> {
    check_recovery(&h.directory)?;
    let directory = h.directory.join("updates");
    std::fs::create_dir_all(&directory)?;
    butterpollo_windows::process::restrict_to_administrators(&directory)?;
    // Reclaim old/cancelled downloads, and failed transaction backups but
    // the newest two. An in-flight cancelled download keeps its file open
    // and cleans itself up.
    let mut transactions = Vec::new();
    for entry in std::fs::read_dir(&directory)?.flatten() {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if uuid::Uuid::parse_str(&name).is_ok() {
            let _ = std::fs::remove_dir_all(entry.path());
        } else {
            transactions.push(name);
        }
    }
    for name in stale_transactions(&transactions) {
        let _ = std::fs::remove_dir_all(directory.join(name));
    }
    // UUID directories avoid partial downloads and overlapping cancellation races.
    let directory = directory.join(id);
    std::fs::create_dir(&directory)?;
    let path = directory.join("setup.exe");
    let result = transfer(h, id, candidate, &path).await;
    if result.is_err() {
        let _ = std::fs::remove_dir_all(directory);
    }
    result
}

fn check_recovery(profile: &std::path::Path) -> Result<Value> {
    let record =
        butterpollo_core::state::load_json(&profile.join("update-result.json"), Value::Null)
            .context("The last update's record is unreadable. Run the Rubylight installer to repair this installation")?;
    if record["phase"] == "recovery_failed"
        || (record["phase"] == "installing" && record["backup"].is_string())
    {
        bail!(
            "An earlier update could not be rolled back. Run the Rubylight installer to repair this installation; keep the updates folder in the profile until it has finished."
        );
    }
    Ok(record)
}

/// Setup's update folders (transaction-<process id>-<nanoseconds>, kept
/// with their backup when an update fails) older than the newest two.
/// Other names are left alone.
fn stale_transactions(names: &[String]) -> Vec<&str> {
    let mut dated: Vec<(u128, &str)> = names
        .iter()
        .filter_map(|name| {
            let (process, time) = name.strip_prefix("transaction-")?.split_once('-')?;
            process.parse::<u32>().ok()?;
            Some((time.parse().ok()?, name.as_str()))
        })
        .collect();
    dated.sort_unstable_by(|a, b| b.cmp(a));
    dated.into_iter().skip(2).map(|(_, name)| name).collect()
}

async fn transfer(
    h: &Shared,
    id: &str,
    candidate: &Installer,
    path: &std::path::Path,
) -> Result<PathBuf> {
    if busy(h) {
        return Err(DownloadPaused.into());
    }
    let mut response = client(Duration::from_secs(600))?
        .get(&candidate.url)
        .send()
        .await?
        .error_for_status()?;
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .await?;
    let mut size = 0;
    let mut hash = Sha256::new();
    while let Some(chunk) = response.chunk().await? {
        if busy(h) {
            return Err(DownloadPaused.into());
        }
        if h.updates.lock().unwrap()["download_id"] != id {
            bail!("Download cancelled");
        }
        size += chunk.len() as u64;
        if size > candidate.size {
            bail!("Installer exceeds its advertised size");
        }
        file.write_all(&chunk).await?;
        hash.update(&chunk);
        h.updates.lock().unwrap()["downloaded_bytes"] = json!(size);
    }
    file.sync_all().await?;
    verify(size, &hex::encode(hash.finalize()), candidate)?;
    if h.updates.lock().unwrap()["download_id"] != id {
        bail!("Download cancelled");
    }
    Ok(path.to_path_buf())
}

#[derive(Debug)]
struct DownloadPaused;
impl std::fmt::Display for DownloadPaused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("download paused while the host is busy")
    }
}
impl std::error::Error for DownloadPaused {}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        host: Shared,
        directory: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir()
                .join(format!("butterpollo-update-test-{}", uuid::Uuid::new_v4()));
            let host = crate::state::Host::load(directory.clone(), directory.join("assets"), None)
                .unwrap();
            Self { host, directory }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
    fn launch(role: butterpollo_core::session::Role) -> crate::state::Launch {
        crate::state::Launch {
            id: "fixture".into(),
            client: butterpollo_core::state::Client {
                name: "fixture".into(),
                cert: String::new(),
                uuid: "fixture".into(),
                perm: u32::MAX,
                enabled: true,
                extra: Default::default(),
            },
            peer: "127.0.0.1".parse().unwrap(),
            app_id: 1,
            key: [0; 16],
            key_id: 1,
            ping: "fixture".into(),
            connect_data: 1,
            role,
            created: std::time::Instant::now(),
            rtsp_encrypted: true,
            rtsp_counter: Default::default(),
            rtsp_received: Default::default(),
            preparation: Default::default(),
            vrr_requested: false,
            host_audio: false,
            requested_rate: 0,
            options: Default::default(),
            audio_preparation: Default::default(),
            preparing: Default::default(),
            warnings: Default::default(),
        }
    }
    #[test]
    fn connections_reset_the_idle_wait_including_during_downloads() {
        use butterpollo_core::{rtsp::Negotiated, session::Role};
        let f = Fixture::new();
        let h = &f.host;
        for role in [Role::Stream, Role::RemoteMonitor, Role::InputOnly] {
            for phase in ["waiting", "downloading", "ready"] {
                *h.updates.lock().unwrap() = json!({"phase":phase,"idle_since":now()-120});
                h.sessions.lock().unwrap().queue(launch(role)).unwrap();
                poll(h);
                assert_eq!(h.updates.lock().unwrap()["phase"], phase);
                assert_eq!(h.updates.lock().unwrap()["idle_since"], 0);
                h.sessions
                    .lock()
                    .unwrap()
                    .start(launch(role), Negotiated::default())
                    .unwrap();
                poll(h);
                assert_eq!(h.updates.lock().unwrap()["idle_since"], 0);
                h.sessions.lock().unwrap().active.clear();
                poll(h);
                assert!(h.updates.lock().unwrap()["idle_since"].as_u64().unwrap() > 0);
                assert_eq!(h.updates.lock().unwrap()["phase"], phase);
            }
        }
    }
    #[test]
    fn cancellation_invalidates_a_download_and_stops_automatic_requeue() {
        let f = Fixture::new();
        let h = &f.host;
        *h.updates.lock().unwrap() =
            json!({"phase":"downloading","download_id":"old","latest_version":"2.1.0"});
        cancel(h).unwrap();
        let state = h.updates.lock().unwrap();
        assert_eq!(state["phase"], "idle");
        assert!(state["download_id"].is_null());
        assert_eq!(state["attempted_version"], "2.1.0");
        drop(state);
        h.updates.lock().unwrap()["phase"] = json!("installing");
        assert!(cancel(h).is_err());
    }
    #[test]
    fn disabling_automatic_updates_cancels_an_automatic_queue_only() {
        let f = Fixture::new();
        let h = &f.host;
        for automatic in [true, false] {
            *h.updates.lock().unwrap() =
                json!({"phase":"waiting","automatic":automatic,"idle_since":0});
            poll(h);
            assert_eq!(
                h.updates.lock().unwrap()["phase"],
                if automatic { "idle" } else { "waiting" }
            );
        }
        assert!(!status(h)["install_supported"].as_bool().unwrap());
        assert!(queue(h, false).is_err());
    }
    #[tokio::test]
    #[ignore = "downloads the official public installer; never executes it"]
    async fn official_release_download_verifies_with_github_digest() -> Result<()> {
        let f = Fixture::new();
        let releases: Vec<Value> = super::releases(&client(Duration::from_secs(35))?)
            .await?
            .json()
            .await?;
        let candidate = releases
            .iter()
            .find_map(|r| installer(r).ok())
            .context("no release installer")?;
        f.host.updates.lock().unwrap()["download_id"] = json!("download-test");
        let path = f.directory.join("setup.exe");
        transfer(&f.host, "download-test", &candidate, &path).await?;
        assert_eq!(std::fs::metadata(&path)?.len(), candidate.size);
        eprintln!(
            "Verified {}: {} bytes, SHA256 {}",
            candidate.version, candidate.size, candidate.digest
        );
        Ok(())
    }
    fn release() -> Value {
        json!({"tag_name":"2.0.0-rc.7","assets":[{"name":"butterpollo-setup-2.0.0-rc.7.exe","browser_download_url":"https://github.com/RamazanKara/Butterpollo/releases/download/2.0.0-rc.7/butterpollo-setup-2.0.0-rc.7.exe","size":32,"digest":format!("sha256:{}", "ab".repeat(32))}]})
    }
    #[test]
    fn an_unresolved_rollback_cannot_be_overwritten_by_another_update() -> Result<()> {
        let f = Fixture::new();
        let path = f.directory.join("update-result.json");
        for phase in ["installing", "recovery_failed"] {
            let record = json!({"version":"2.0.0-rc.22","phase":phase,
                "backup":f.directory.join("updates/transaction-1-2/previous")});
            butterpollo_core::state::write_json(&path, &record)?;
            assert!(check_recovery(&f.directory).is_err());
            assert_eq!(
                butterpollo_core::state::load_json(&path, Value::Null)?,
                record
            );
        }
        for phase in ["installed", "rolled_back", "failed", "superseded"] {
            butterpollo_core::state::write_json(&path, &json!({"phase":phase}))?;
            assert!(check_recovery(&f.directory).is_ok());
        }
        std::fs::write(&path, b"{")?;
        let error = check_recovery(&f.directory).unwrap_err().to_string();
        assert!(error.contains("Run the Rubylight installer"), "{error}");
        Ok(())
    }
    #[tokio::test]
    async fn busy_host_defers_download_before_contacting_the_network() {
        let f = Fixture::new();
        f.host
            .sessions
            .lock()
            .unwrap()
            .queue(launch(butterpollo_core::session::Role::Stream))
            .unwrap();
        let candidate = installer(&release()).unwrap();
        let error = transfer(&f.host, "test", &candidate, &f.directory.join("setup.exe"))
            .await
            .unwrap_err();
        assert!(error.is::<DownloadPaused>());
        assert!(!f.directory.join("setup.exe").exists());
    }
    #[test]
    fn only_exact_release_assets_with_digests_can_be_installed() {
        assert!(installer(&release()).is_ok());
        for (key, value) in [
            (
                "browser_download_url",
                json!("https://example.org/setup.exe"),
            ),
            ("digest", Value::Null),
            ("size", json!(MAX_INSTALLER + 1)),
            ("name", json!("butterpollo-setup-2.0.0-rc.7-candidate.exe")),
        ] {
            let mut release = release();
            release["assets"][0][key] = value;
            assert!(installer(&release).is_err(), "{key}");
        }
    }
    #[test]
    fn installers_from_before_and_after_the_rename_are_accepted() {
        for (repository, name) in [
            ("Rubylight", "rubylight-setup-2.0.0-rc.7.exe"),
            ("Rubylight", "butterpollo-setup-2.0.0-rc.7.exe"),
            ("Butterpollo", "rubylight-setup-2.0.0-rc.7.exe"),
            ("Butterpollo", "butterpollo-setup-2.0.0-rc.7.exe"),
        ] {
            let url = format!(
                "https://github.com/RamazanKara/{repository}/releases/download/2.0.0-rc.7/{name}"
            );
            let mut release = release();
            release["assets"][0]["name"] = json!(name);
            release["assets"][0]["browser_download_url"] = json!(url);
            assert_eq!(installer(&release).unwrap().url, url);
        }
        // The current name wins when a release carries both.
        let mut both = release();
        let mut current = both["assets"][0].clone();
        current["name"] = json!("rubylight-setup-2.0.0-rc.7.exe");
        current["browser_download_url"] = json!(
            "https://github.com/RamazanKara/Rubylight/releases/download/2.0.0-rc.7/rubylight-setup-2.0.0-rc.7.exe"
        );
        both["assets"].as_array_mut().unwrap().push(current);
        assert!(
            installer(&both)
                .unwrap()
                .url
                .ends_with("/rubylight-setup-2.0.0-rc.7.exe")
        );
        // Another repository's copy is refused.
        let mut foreign = release();
        foreign["assets"][0]["browser_download_url"] = json!(
            "https://github.com/someone/Rubylight/releases/download/2.0.0-rc.7/butterpollo-setup-2.0.0-rc.7.exe"
        );
        assert!(installer(&foreign).is_err());
    }
    #[test]
    fn only_the_two_newest_update_transactions_are_kept() {
        let names: Vec<String> = [
            "transaction-9876-1700000000000000300",
            "transaction-12-1700000000000000100",
            "transaction-4-1700000000000000400",
            "transaction-123456-1700000000000000200",
            "transaction-x-1700000000000000000",
            "transaction-5-",
            "backup",
        ]
        .map(String::from)
        .into();
        let mut stale = stale_transactions(&names);
        stale.sort_unstable();
        assert_eq!(
            stale,
            [
                "transaction-12-1700000000000000100",
                "transaction-123456-1700000000000000200"
            ]
        );
        assert!(stale_transactions(&names[..2]).is_empty());
    }
    #[test]
    fn truncated_or_changed_installer_is_rejected() {
        let candidate = installer(&release()).unwrap();
        assert!(verify(32, &candidate.digest, &candidate).is_ok());
        assert!(verify(31, &candidate.digest, &candidate).is_err());
        assert!(verify(32, &"00".repeat(32), &candidate).is_err());
    }
}
