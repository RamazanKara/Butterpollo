//! In-place service updates. The verified package is staged before shutdown;
//! changed files are backed up and restored if copying or startup fails.
use crate::{detect, install, log::line, payload, system, ui::Progress};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};

pub fn run(folder: &Path, start: bool, progress: &Progress) -> Result<()> {
    detect::scan().check_version()?;
    let profile = install::profile();
    let result = profile.join("update-result.json");
    let install = std::fs::canonicalize(folder)?;
    let service =
        system::service_program(detect::SERVICE).context("Butterpollo service is not installed")?;
    check_service_folder(&service, &install)?;
    // A non-shared handle rejects another updater until this transaction ends.
    use std::os::windows::fs::OpenOptionsExt;
    let _lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .share_mode(0)
        .open(profile.join("update.lock"))
        .context("Another update is already running")?;
    // The new backup must hold one whole version, not what an interrupted
    // update left.
    install::ensure_idle(install::probe(&profile))?;
    recover_locked(&result).context("The update that did not finish could not be rolled back")?;
    let work = profile.join("updates").join(format!(
        "transaction-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&work)?;
    system::restrict(&work, false)?;
    let staged = work.join("package");
    let backup = work.join("previous");
    std::fs::create_dir(&staged)?;
    progress.set("Verifying the update…");
    payload::Payload::open()?
        .context("The update installer has no package")?
        .extract(&staged)?;
    let entries = payload::verify(&staged)?;
    let previous = payload::manifest(&install)
        .context("The installed package has no readable manifest; restore manifest.json from that release before upgrading")?;
    let paths = previous
        .iter()
        .chain(&entries)
        .map(|e| e.path.clone())
        .chain(["manifest.json".into(), "uninstall.exe".into()])
        .collect::<BTreeSet<_>>();
    install::ensure_idle(install::probe(&profile))?;
    write_result(&result, "installing", None)?;
    progress.set("Stopping Butterpollo…");
    stop_for_update(
        &result,
        || system::stop_service(detect::SERVICE),
        || {
            if !system::wait_stopped(detect::SERVICE, Duration::from_secs(120)) {
                line("the service is still stopping");
            }
            system::start_service(detect::SERVICE)
        },
    )?;
    // Nothing has been overwritten if the backup fails.
    let saved = Backup::create(&install, &backup, &paths).and_then(|saved| {
        // From here files change. If this setup cannot finish, setup or
        // the service rolls back from this backup the next time it starts.
        write_record(
            &result,
            &json!({"version":env!("CARGO_PKG_VERSION"),"phase":"installing","error":null,
                    "backup":backup,"install":install}),
        )?;
        Ok(saved)
    });
    let saved = match saved {
        Ok(saved) => saved,
        Err(error) => {
            let _ = system::start_service(detect::SERVICE);
            write_result(&result, "failed", Some(&format!("{error:#}")))?;
            return Err(error);
        }
    };
    let update = attempt_install(
        &saved,
        &install,
        || -> Result<()> {
            progress.set("Installing the update…");
            for entry in &entries {
                install::replace_file(
                    &payload::safe_join(&staged, &entry.path)?,
                    &payload::safe_join(&install, &entry.path)?,
                )?;
            }
            install::replace_file(
                &staged.join("manifest.json"),
                &install.join("manifest.json"),
            )?;
            payload::write_stub(&install.join("uninstall.exe"))?;
            progress.set("Checking that Butterpollo starts…");
            if start {
                system::start_service(detect::SERVICE)?;
                install::wait_ready(install::probe(&profile), Some(env!("CARGO_PKG_VERSION")))?;
            }
            install::register(&install, &entries)?;
            Ok(())
        },
        || {
            progress.set("Restoring the previous version…");
            system::stop_service(detect::SERVICE)
        },
        || {
            system::start_service(detect::SERVICE)?;
            install::wait_ready(install::probe(&profile), None)
        },
    );
    match update {
        Ok(()) => {
            write_result(&result, "installed", None)?;
            for old in &previous {
                if !entries
                    .iter()
                    .any(|e| e.path.eq_ignore_ascii_case(&old.path))
                {
                    let path = payload::safe_join(&install, &old.path)?;
                    let _ = std::fs::remove_file(path);
                }
            }
            let _ = std::fs::remove_dir_all(&work);
            Ok(())
        }
        Err(InstallFailure {
            error,
            recovery: rollback,
        }) => {
            let message = match &rollback {
                Ok(()) => format!("Update failed; the previous version was restored. {error:#}"),
                Err(rollback) => format!(
                    "Update failed: {error:#}. Recovery failed: {rollback:#}. Backup: {}",
                    backup.display()
                ),
            };
            let phase = if rollback.is_ok() {
                "rolled_back"
            } else {
                "recovery_failed"
            };
            write_record(
                &result,
                &json!({"version":env!("CARGO_PKG_VERSION"),"phase":phase,"error":message,
                        "backup":backup,"install":install}),
            )?;
            bail!("{message}")
        }
    }
}

fn check_service_folder(service: &Path, install: &Path) -> Result<()> {
    // The executable itself can be missing after an interrupted replacement.
    if !service
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("butterpollo-service.exe"))
        || std::fs::canonicalize(service.parent().context("service program has no folder")?)?
            != install
    {
        bail!("The update folder does not belong to the installed Butterpollo service");
    }
    Ok(())
}

/// Roll back an update that power loss or a crash interrupted, unless a
/// setup is running (it holds update.lock). See recover_locked.
pub fn recover(profile: &Path) -> Result<()> {
    let result = profile.join("update-result.json");
    if !result.is_file() {
        return Ok(());
    }
    use std::os::windows::fs::OpenOptionsExt;
    let _lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .share_mode(0)
        .open(profile.join("update.lock"))
        .context("Cannot recover the previous update; another setup may still be running")?;
    recover_locked(&result)
}
/// With update.lock held: if the last update still says "installing" after
/// its backup was made, its setup never finished, so the files the backup
/// holds are put back. The service does the same when it starts
/// (butterpollo_core::update_recovery); keep the two in step.
fn recover_locked(result: &Path) -> Result<()> {
    let bytes = match std::fs::read(result) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut record: serde_json::Value = serde_json::from_slice(&bytes).context(
        "The update recovery record cannot be read; keep the updates folder for recovery",
    )?;
    let (Some("installing" | "recovery_failed"), Some(backup), Some(install)) = (
        record["phase"].as_str(),
        record["backup"].as_str(),
        record["install"].as_str(),
    ) else {
        return Ok(());
    };
    line(format!(
        "the update to {} did not finish; restoring {install} from {backup}",
        record["version"]
    ));
    let restored =
        Backup::load(Path::new(backup)).and_then(|saved| saved.restore(Path::new(install)));
    let (phase, message) = match &restored {
        Ok(()) => (
            "rolled_back",
            "The update did not finish; the previous version was restored.".to_owned(),
        ),
        Err(error) => (
            "recovery_failed",
            format!(
                "The update did not finish, and restoring the previous version failed: {error:#}. Backup: {backup}"
            ),
        ),
    };
    record["phase"] = json!(phase);
    record["error"] = json!(message);
    write_record(result, &record)?;
    restored
}
/// Stop the service before any file changes. If it does not stop in time,
/// the update fails before changing anything, and the service is started
/// again once it has stopped, so the host is not left stopped.
fn stop_for_update(
    result: &Path,
    stop: impl FnOnce() -> Result<()>,
    start_again: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let Err(error) = stop() else {
        return Ok(());
    };
    if let Err(start) = start_again() {
        line(format!("the service could not be started again: {start:#}"));
    }
    write_result(result, "failed", Some(&format!("{error:#}")))?;
    Err(error)
}

#[derive(Debug)]
struct InstallFailure {
    error: anyhow::Error,
    recovery: Result<()>,
}

/// The same recovery path is exercised with isolated files and a stand-in
/// service lifecycle in tests; it never overwrites files if stopping fails.
fn attempt_install(
    saved: &Backup,
    install: &Path,
    apply: impl FnOnce() -> Result<()>,
    stop: impl FnOnce() -> Result<()>,
    restart: impl FnOnce() -> Result<()>,
) -> std::result::Result<(), InstallFailure> {
    if let Err(error) = apply() {
        let recovery = (|| {
            stop()?;
            saved.restore(install)?;
            restart()
        })();
        Err(InstallFailure { error, recovery })
    } else {
        Ok(())
    }
}

fn write_result(path: &Path, phase: &str, error: Option<&str>) -> Result<()> {
    write_record(
        path,
        &json!({"version":env!("CARGO_PKG_VERSION"),"phase":phase,"error":error}),
    )
}
fn write_record(path: &Path, value: &serde_json::Value) -> Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::File::create(&temporary)?;
    std::io::Write::write_all(&mut file, &serde_json::to_vec_pretty(value)?)?;
    // A record that names a backup must survive a power loss.
    file.sync_all()?;
    drop(file);
    // The console may read this file during startup. Replace it atomically.
    use windows::{
        Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        core::HSTRING,
    };
    unsafe {
        MoveFileExW(
            &HSTRING::from(temporary.as_os_str()),
            &HSTRING::from(path.as_os_str()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}

struct Backup {
    directory: PathBuf,
    files: Vec<(String, bool)>,
}
impl Backup {
    fn create(install: &Path, directory: &Path, paths: &BTreeSet<String>) -> Result<Self> {
        // On disk before any file is replaced, so that a power loss cannot
        // leave a rollback with a backup that was never written.
        let flush = |path: &Path| -> Result<()> {
            Ok(std::fs::OpenOptions::new()
                .write(true)
                .open(path)?
                .sync_all()?)
        };
        let mut files = Vec::new();
        for name in paths {
            let source = payload::safe_join(install, name)?;
            let target = payload::safe_join(directory, name)?;
            if source.exists() {
                std::fs::create_dir_all(target.parent().context("backup path has no parent")?)?;
                std::fs::copy(source, &target)?;
                flush(&target)?;
                files.push((name.clone(), true));
            } else {
                files.push((name.clone(), false));
            }
        }
        std::fs::write(directory.join("backup.json"), serde_json::to_vec(&files)?)?;
        flush(&directory.join("backup.json"))?;
        Ok(Self {
            directory: directory.to_path_buf(),
            files,
        })
    }
    fn load(directory: &Path) -> Result<Self> {
        let files: Vec<(String, bool)> =
            serde_json::from_slice(&std::fs::read(directory.join("backup.json"))?)
                .context("reading the update backup")?;
        // Every saved file is checked before any is put back.
        for (name, existed) in &files {
            let path = payload::safe_join(directory, name)?;
            if *existed && !path.is_file() {
                bail!("the update backup lacks {name}");
            }
        }
        Ok(Self {
            directory: directory.to_path_buf(),
            files,
        })
    }
    fn restore(&self, install: &Path) -> Result<()> {
        for (name, existed) in &self.files {
            let target = payload::safe_join(install, name)?;
            if *existed {
                install::replace_file(&payload::safe_join(&self.directory, name)?, &target)?;
            } else if target.exists() {
                std::fs::remove_file(target)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_missing_service_executable_does_not_prevent_recovery() -> Result<()> {
        let root = tempfile::tempdir()?;
        let install = root.path().join("installed");
        std::fs::create_dir(&install)?;
        let install = install.canonicalize()?;
        let service = install.join("butterpollo-service.exe");
        std::fs::write(&service, b"previous service")?;
        let backup = root.path().join("backup");
        Backup::create(
            &install,
            &backup,
            &["butterpollo-service.exe".into()].into_iter().collect(),
        )?;
        let record = root.path().join("update-result.json");
        write_record(
            &record,
            &json!({"phase":"installing","backup":backup,"install":install}),
        )?;
        std::fs::remove_file(&service)?;
        check_service_folder(&service, &install)?;
        recover_locked(&record)?;
        assert_eq!(std::fs::read(service)?, b"previous service");
        assert!(check_service_folder(&install.join("another-service.exe"), &install).is_err());
        assert!(check_service_folder(&backup.join("butterpollo-service.exe"), &install).is_err());
        Ok(())
    }
    #[test]
    fn rc_upgrades_and_reinstalls_keep_profiles_and_recover_each_interrupted_copy() -> Result<()> {
        let root = tempfile::tempdir()?;
        let install = root.path().join("Çağrı Müller/installed");
        let profile = root.path().join("Çağrı Müller/config");
        let staged = root.path().join("staged");
        std::fs::create_dir_all(install.join("assets/web"))?;
        std::fs::create_dir_all(staged.join("assets/web"))?;
        std::fs::create_dir_all(profile.join("credentials"))?;
        let profile_files = [
            "sunshine.conf",
            "sunshine_state.json",
            "sunshine_credentials.json",
            "apps.json",
            "vibeshine_state.json",
            "credentials/cacert.pem",
            "credentials/cakey.pem",
            "display-state.json",
            "cover.png",
        ];
        for name in profile_files {
            std::fs::write(profile.join(name), format!("user-owned {name}"))?;
        }
        let files = [
            "butterpollo.exe",
            "butterpollo-service.exe",
            "assets/web/index.html",
        ];
        let paths = files
            .iter()
            .map(|name| (*name).into())
            .chain(["new.dll".into()])
            .collect();
        for version in ["2.0.0-rc.20", "2.0.0-rc.21"] {
            for copied in 0..=files.len() {
                for name in files {
                    std::fs::write(install.join(name), "2.0.0-rc.20")?;
                    std::fs::write(staged.join(name), version)?;
                }
                let backup = root.path().join(format!("previous-{version}-{copied}"));
                let saved = Backup::create(&install, &backup, &paths)?;
                let result = profile.join("update-result.json");
                write_record(
                    &result,
                    &json!({"version":version,"phase":"installing","backup":backup,"install":install}),
                )?;
                for name in files.iter().take(copied) {
                    install::replace_file(&staged.join(name), &install.join(name))?;
                }
                std::fs::write(install.join("new.dll"), b"new")?;
                recover_locked(&result)?;
                for name in files {
                    assert_eq!(std::fs::read(install.join(name))?, b"2.0.0-rc.20");
                }
                assert!(!install.join("new.dll").exists());
                attempt_install(
                    &saved,
                    &install,
                    || {
                        for name in files {
                            install::replace_file(&staged.join(name), &install.join(name))?;
                        }
                        Ok(())
                    },
                    || panic!("successful update must not roll back"),
                    || panic!(),
                )
                .map_err(|failure| failure.error)?;
                for name in files {
                    assert_eq!(std::fs::read(install.join(name))?, version.as_bytes());
                }
                for name in profile_files {
                    assert_eq!(
                        std::fs::read(profile.join(name))?,
                        format!("user-owned {name}").as_bytes()
                    );
                }
            }
        }
        Ok(())
    }
    #[test]
    fn startup_failure_restores_files_before_restarting_the_previous_host() -> Result<()> {
        let root = tempfile::tempdir()?;
        let installed = root.path().join("installed");
        std::fs::create_dir(&installed)?;
        std::fs::write(installed.join("host.exe"), b"previous")?;
        let backup = Backup::create(
            &installed,
            &root.path().join("backup"),
            &["host.exe".into()].into_iter().collect(),
        )?;
        let stopped = std::cell::Cell::new(false);
        let failure = attempt_install(
            &backup,
            &installed,
            || {
                std::fs::write(installed.join("host.exe"), b"update")?;
                bail!("new host failed its startup health check")
            },
            || {
                stopped.set(true);
                Ok(())
            },
            || {
                assert!(stopped.get());
                assert_eq!(std::fs::read(installed.join("host.exe"))?, b"previous");
                Ok(())
            },
        )
        .unwrap_err();
        assert!(failure.recovery.is_ok());
        assert!(failure.error.to_string().contains("startup health check"));
        Ok(())
    }
    #[test]
    fn recovery_does_not_replace_files_if_the_service_cannot_be_stopped() -> Result<()> {
        let root = tempfile::tempdir()?;
        let installed = root.path().join("installed");
        std::fs::create_dir(&installed)?;
        std::fs::write(installed.join("host.exe"), b"previous")?;
        let backup = Backup::create(
            &installed,
            &root.path().join("backup"),
            &["host.exe".into()].into_iter().collect(),
        )?;
        let failure = attempt_install(
            &backup,
            &installed,
            || {
                std::fs::write(installed.join("host.exe"), b"update")?;
                bail!("startup failed")
            },
            || bail!("stop failed"),
            || panic!("must not start after a failed stop"),
        )
        .unwrap_err();
        assert!(failure.recovery.is_err());
        assert_eq!(std::fs::read(installed.join("host.exe"))?, b"update");
        assert_eq!(
            std::fs::read(root.path().join("backup/host.exe"))?,
            b"previous"
        );
        Ok(())
    }
    #[test]
    fn a_service_that_does_not_stop_is_started_again_and_the_update_fails() -> Result<()> {
        let root = tempfile::tempdir()?;
        let result = root.path().join("update-result.json");
        write_result(&result, "installing", None)?;
        let started = std::cell::Cell::new(false);
        let error = stop_for_update(
            &result,
            || bail!("the ApolloService service did not stop"),
            || {
                started.set(true);
                bail!("still stopping")
            },
        )
        .unwrap_err();
        assert!(started.get());
        assert!(error.to_string().contains("did not stop"));
        let written: serde_json::Value = serde_json::from_slice(&std::fs::read(&result)?)?;
        assert_eq!(written["phase"], "failed");
        assert!(written["error"].as_str().unwrap().contains("did not stop"));
        stop_for_update(
            &result,
            || Ok(()),
            || panic!("a stopped service is not started"),
        )?;
        Ok(())
    }
    #[test]
    fn an_interrupted_update_is_rolled_back_when_setup_starts() -> Result<()> {
        let root = tempfile::tempdir()?;
        let profile = root.path().join("config");
        let installed = root.path().join("installed");
        let backup = profile.join("updates/transaction-1-2/previous");
        std::fs::create_dir_all(&backup)?;
        std::fs::create_dir(&installed)?;
        std::fs::write(installed.join("host.exe"), b"previous")?;
        Backup::create(
            &installed,
            &backup,
            &["host.exe".into(), "new.dll".into()].into_iter().collect(),
        )?;
        std::fs::write(installed.join("host.exe"), b"half")?;
        std::fs::write(installed.join("new.dll"), b"new")?;
        let result = profile.join("update-result.json");
        write_record(
            &result,
            &json!({"version":"2.0.1","phase":"installing","error":null,
                    "backup":backup,"install":installed}),
        )?;
        {
            // A setup that holds the lock is still at work.
            use std::os::windows::fs::OpenOptionsExt;
            let _held = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .share_mode(0)
                .open(profile.join("update.lock"))?;
            assert!(recover(&profile).is_err());
            assert_eq!(std::fs::read(installed.join("host.exe"))?, b"half");
        }
        recover(&profile)?;
        assert_eq!(std::fs::read(installed.join("host.exe"))?, b"previous");
        assert!(!installed.join("new.dll").exists());
        let written: serde_json::Value = serde_json::from_slice(&std::fs::read(&result)?)?;
        assert_eq!(written["phase"], "rolled_back");
        assert_eq!(written["version"], "2.0.1");
        // Before the backup was made, no file had changed.
        std::fs::write(installed.join("host.exe"), b"later")?;
        write_result(&result, "installing", None)?;
        recover(&profile)?;
        assert_eq!(std::fs::read(installed.join("host.exe"))?, b"later");
        Ok(())
    }
    #[test]
    fn failed_recovery_keeps_the_backup_and_retries() -> Result<()> {
        let root = tempfile::tempdir()?;
        let install = root.path().join("install");
        let backup = root.path().join("backup");
        std::fs::create_dir(&install)?;
        std::fs::create_dir(&backup)?;
        std::fs::write(install.join("host.exe"), b"half")?;
        std::fs::write(backup.join("backup.json"), r#"[["host.exe",true]]"#)?;
        let result = root.path().join("update-result.json");
        write_record(
            &result,
            &json!({"version":"2.0.0-rc.22","phase":"installing",
            "backup":backup,"install":install}),
        )?;
        assert!(recover_locked(&result).is_err());
        let record: serde_json::Value = serde_json::from_slice(&std::fs::read(&result)?)?;
        assert_eq!(record["phase"], "recovery_failed");
        assert_eq!(record["backup"], json!(backup));
        assert_eq!(record["install"], json!(install));
        std::fs::write(backup.join("host.exe"), b"previous")?;
        recover_locked(&result)?;
        assert_eq!(std::fs::read(install.join("host.exe"))?, b"previous");
        std::fs::write(&result, b"broken record")?;
        assert!(recover_locked(&result).is_err());
        assert_eq!(std::fs::read(&result)?, b"broken record");
        Ok(())
    }
    #[test]
    fn rollback_restores_removed_and_overwritten_files_and_removes_new_files() -> Result<()> {
        let root = tempfile::tempdir()?;
        let installed = root.path().join("installed");
        std::fs::create_dir(&installed)?;
        std::fs::write(installed.join("host.exe"), b"old executable")?;
        std::fs::write(installed.join("old.dll"), b"old dll")?;
        std::fs::write(installed.join("settings.conf"), b"user settings")?;
        let paths = ["host.exe", "old.dll", "new.dll"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let backup = Backup::create(&installed, &root.path().join("backup"), &paths)?;
        std::fs::write(installed.join("host.exe"), b"broken new executable")?;
        std::fs::remove_file(installed.join("old.dll"))?;
        std::fs::write(installed.join("new.dll"), b"new dll")?;
        backup.restore(&installed)?;
        assert_eq!(
            std::fs::read(installed.join("host.exe"))?,
            b"old executable"
        );
        assert_eq!(std::fs::read(installed.join("old.dll"))?, b"old dll");
        assert!(!installed.join("new.dll").exists());
        assert_eq!(
            std::fs::read(installed.join("settings.conf"))?,
            b"user settings"
        );
        Ok(())
    }
}
