//! In-place service updates. The verified package is staged before shutdown;
//! changed files are backed up and restored if copying or startup fails.
use crate::{detect, install, payload, system, ui::Progress};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub fn run(folder: &Path, progress: &Progress) -> Result<()> {
    let profile = install::profile();
    let result = profile.join("update-result.json");
    let install = std::fs::canonicalize(folder)?;
    let service =
        system::service_program(detect::SERVICE).context("Butterpollo service is not installed")?;
    if std::fs::canonicalize(service)? != install.join("butterpollo-service.exe") {
        bail!("The update folder does not belong to the installed Butterpollo service");
    }
    // A non-shared handle rejects another updater until this transaction ends.
    use std::os::windows::fs::OpenOptionsExt;
    let _lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .share_mode(0)
        .open(profile.join("update.lock"))
        .context("Another update is already running")?;
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
    for required in [
        "butterpollo.exe",
        "butterpollo-service.exe",
        "Start Butterpollo.exe",
    ] {
        if !entries.iter().any(|e| e.path == required) {
            bail!("The update is missing {required}");
        }
    }
    let previous = payload::manifest(&install)
        .context("The installed package has no manifest; run the installer manually")?;
    let paths = previous
        .iter()
        .chain(&entries)
        .map(|e| e.path.clone())
        .chain(["manifest.json".into(), "uninstall.exe".into()])
        .collect::<BTreeSet<_>>();
    write_result(&result, "installing", None)?;
    progress.set("Stopping Butterpollo…");
    system::stop_service(detect::SERVICE)?;
    // Nothing has been overwritten if the backup fails.
    let saved = match Backup::create(&install, &backup, &paths) {
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
            system::start_service(detect::SERVICE)?;
            install::wait_ready(
                install::web_port(&profile) - 1,
                Some(env!("CARGO_PKG_VERSION")),
            )?;
            install::register(&install, &entries)?;
            Ok(())
        },
        || {
            progress.set("Restoring the previous version…");
            system::stop_service(detect::SERVICE)
        },
        || {
            system::start_service(detect::SERVICE)?;
            install::wait_ready(install::web_port(&profile) - 1, None)
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
            write_result(
                &result,
                if rollback.is_ok() {
                    "rolled_back"
                } else {
                    "recovery_failed"
                },
                Some(&message),
            )?;
            bail!("{message}")
        }
    }
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
    let value = json!({"version":env!("CARGO_PKG_VERSION"),"phase":phase,"error":error});
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
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
        let mut files = Vec::new();
        for name in paths {
            let source = payload::safe_join(install, name)?;
            let target = payload::safe_join(directory, name)?;
            if source.exists() {
                std::fs::create_dir_all(target.parent().context("backup path has no parent")?)?;
                std::fs::copy(source, target)?;
                files.push((name.clone(), true));
            } else {
                files.push((name.clone(), false));
            }
        }
        std::fs::write(directory.join("backup.json"), serde_json::to_vec(&files)?)?;
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
