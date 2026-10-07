//! Removing Butterpollo. Settings and drivers stay unless asked otherwise.
use crate::{
    detect::{self, SERVICE, UNINSTALL},
    install::{HOST_PROCESSES, profile, start_menu_link},
    log::line,
    payload, system,
    ui::Progress,
};
use anyhow::{Result, bail};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY};

pub struct Options {
    pub factory_reset: bool,
    pub remove_drivers: bool,
}
/// The installation to remove: the registered one, else the folder this
/// program runs from when it holds a Butterpollo package. Never a guess:
/// run from Downloads on a PC without Butterpollo, this is None.
pub fn install_location() -> Option<PathBuf> {
    let found = detect::scan();
    found
        .butterpollo
        .as_ref()
        .and_then(|p| p.location.clone())
        .or(found.service_install)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf))
                .filter(|folder| is_package(folder))
        })
}
/// Whether `folder` holds an installed Butterpollo package.
fn is_package(folder: &Path) -> bool {
    payload::manifest(folder).is_ok_and(|entries| {
        entries
            .iter()
            .any(|entry| entry.path.eq_ignore_ascii_case("butterpollo.exe"))
    })
}
pub fn uninstall(options: &Options, progress: &Progress) -> Result<()> {
    let Some(install) = install_location() else {
        bail!("Butterpollo is not installed on this PC");
    };
    line(format!("uninstalling from {}", install.display()));
    progress.set("Stopping Butterpollo…");
    let _ = system::stop_service(SERVICE);
    system::kill(&HOST_PROCESSES[..3]);
    if system::service_program(SERVICE).is_some_and(|p| p.starts_with(&install)) {
        system::delete_service(SERVICE)?;
    }
    system::firewall_remove("Butterpollo");

    // The host registers its HDR Vulkan layer while it runs.
    let layers = "SOFTWARE\\Khronos\\Vulkan\\ImplicitLayers";
    let prefix = install.display().to_string().to_ascii_lowercase();
    for value in system::values(HKEY_LOCAL_MACHINE, layers, KEY_WOW64_64KEY) {
        if value.to_ascii_lowercase().starts_with(&prefix) {
            system::delete_value(HKEY_LOCAL_MACHINE, layers, &value, KEY_WOW64_64KEY);
        }
    }

    if options.remove_drivers {
        progress.set("Removing the virtual drivers…");
        let powershell = format!(
            "{}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
            std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into())
        );
        for (script, args) in [
            (
                "drivers\\gamepad\\cleanup.ps1",
                vec!["-InstallerBestEffort", "-RemoveDriverStorePackage:1"],
            ),
            ("drivers\\display\\install.ps1", vec!["-Uninstall"]),
        ] {
            let path = install.join(script);
            if path.is_file() {
                let path = path.display().to_string();
                let mut arguments = vec![
                    "-NoLogo",
                    "-NonInteractive",
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                    path.as_str(),
                ];
                arguments.extend(args);
                let _ = system::run_as_system(&powershell, &arguments, Duration::from_secs(600));
            }
        }
    }

    progress.set("Removing files…");
    let _ = std::fs::remove_file(start_menu_link());
    system::delete_key(HKEY_LOCAL_MACHINE, &format!("{UNINSTALL}\\Butterpollo"));
    remove_package(&install);
    if options.factory_reset {
        progress.set("Deleting settings and paired devices…");
        let _ = std::fs::remove_dir_all(profile().parent().unwrap_or(&profile()));
    }
    // This program may run from the folder being removed: delete it once it
    // has exited, then the folder if nothing else is left in it (rd without
    // /s refuses a folder that is not empty).
    if let Ok(exe) = std::env::current_exe()
        && exe.starts_with(&install)
    {
        let command = format!(
            "ping 127.0.0.1 -n 3 > nul & del /f /q \"{}\" & rd \"{}\"",
            exe.display(),
            install.display()
        );
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new(system::system32("cmd.exe"))
            .args(["/D", "/C", &command])
            .creation_flags(0x0800_0000 | 0x0000_0008)
            .spawn();
    } else {
        let _ = std::fs::remove_dir(&install);
    }
    Ok(())
}
/// Delete what the installer put in `install`: the manifest's files, the
/// manifest, the uninstaller and driver folders carried over from Vibepollo.
/// Anything else stays, and so does every folder that is not empty then: the
/// install folder may also hold the user's own files.
fn remove_package(install: &Path) {
    let mut folders = BTreeSet::new();
    let mut remove = |path: PathBuf| {
        folders.extend(
            path.ancestors()
                .skip(1)
                .take_while(|folder| *folder != install && folder.starts_with(install))
                .map(Path::to_path_buf),
        );
        system::remove_file_later(&path);
    };
    if let Ok(entries) = payload::manifest(install) {
        for entry in entries {
            if let Ok(path) = payload::safe_join(install, &entry.path) {
                remove(path);
            }
        }
    }
    // Drivers carried over from Vibepollo are not in the manifest; only the
    // installer's own driver folders go, recognised by their script.
    for folder in ["drivers\\display", "drivers\\gamepad"] {
        let folder = install.join(folder);
        if folder.join("install.ps1").is_file()
            && let Ok(files) = files_in(&folder)
        {
            files.into_iter().for_each(&mut remove);
        }
    }
    remove(install.join("manifest.json"));
    remove(install.join("uninstall.exe"));
    // Deepest first, so a parent is empty by the time it comes up.
    for folder in folders.iter().rev() {
        let _ = std::fs::remove_dir(folder);
    }
}
fn files_in(folder: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            files.extend(files_in(&entry.path())?);
        } else {
            files.push(entry.path());
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fn package(root: &Path) {
        write(
            &root.join("manifest.json"),
            r#"[{"path":"butterpollo.exe","sha256":""},{"path":"assets/web/index.html","sha256":""}]"#,
        );
        write(&root.join("butterpollo.exe"), "exe");
        write(&root.join("assets\\web\\index.html"), "html");
        write(&root.join("uninstall.exe"), "stub");
        write(&root.join("drivers\\display\\install.ps1"), "script");
        write(&root.join("drivers\\display\\driver.inf"), "inf");
    }
    #[test]
    fn only_a_folder_holding_the_package_counts_as_an_installation() {
        let downloads = tempfile::tempdir().unwrap();
        write(&downloads.path().join("holiday.jpg"), "photo");
        assert!(!is_package(downloads.path()));
        write(
            &downloads.path().join("manifest.json"),
            r#"[{"path":"other.exe","sha256":""}]"#,
        );
        assert!(!is_package(downloads.path()));
        let install = tempfile::tempdir().unwrap();
        package(install.path());
        assert!(is_package(install.path()));
    }
    #[test]
    fn uninstalling_removes_the_package_but_keeps_the_users_own_files() {
        let install = tempfile::tempdir().unwrap();
        let root = install.path();
        package(root);
        write(&root.join("notes.txt"), "mine");
        write(&root.join("assets\\mine.png"), "mine");
        write(&root.join("drivers\\other\\tool.sys"), "mine");
        std::fs::create_dir_all(root.join("empty")).unwrap();
        remove_package(root);
        for gone in [
            "butterpollo.exe",
            "assets\\web\\index.html",
            "assets\\web",
            "manifest.json",
            "uninstall.exe",
            "drivers\\display",
        ] {
            assert!(!root.join(gone).exists(), "{gone} remains");
        }
        for kept in [
            "notes.txt",
            "assets\\mine.png",
            "drivers\\other\\tool.sys",
            "empty",
        ] {
            assert!(root.join(kept).exists(), "{kept} was deleted");
        }
    }
    #[test]
    fn an_installation_of_only_the_package_leaves_nothing_behind() {
        let install = tempfile::tempdir().unwrap();
        let root = install.path().join("Butterpollo");
        package(&root);
        remove_package(&root);
        std::fs::remove_dir(&root).unwrap();
        assert!(!root.exists());
    }
}
