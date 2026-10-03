//! Removing Butterpollo. Settings and drivers stay unless asked otherwise.
use crate::{
    detect::{self, SERVICE, UNINSTALL},
    install::{HOST_PROCESSES, profile, start_menu_link},
    log::line,
    payload, system,
    ui::Progress,
};
use anyhow::Result;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY};

pub struct Options {
    pub factory_reset: bool,
    pub remove_drivers: bool,
}
pub fn install_location() -> PathBuf {
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
        })
        .unwrap_or_else(|| system::program_files().join("Butterpollo"))
}
pub fn uninstall(options: &Options, progress: &Progress) -> Result<()> {
    let install = install_location();
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
    if let Ok(entries) = payload::manifest(&install) {
        for entry in entries {
            if let Ok(path) = payload::safe_join(&install, &entry.path) {
                system::remove_file_later(&path);
            }
        }
    }
    let _ = std::fs::remove_dir_all(install.join("drivers"));
    system::remove_file_later(&install.join("manifest.json"));
    remove_empty_folders(&install);
    if options.factory_reset {
        progress.set("Deleting settings and paired devices…");
        let _ = std::fs::remove_dir_all(profile().parent().unwrap_or(&profile()));
    }
    // This program may run from the folder being removed.
    if let Ok(exe) = std::env::current_exe()
        && exe.starts_with(&install)
    {
        let command = format!(
            "ping 127.0.0.1 -n 3 > nul & del /f /q \"{}\" & rd /s /q \"{}\"",
            exe.display(),
            install.display()
        );
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new(system::system32("cmd.exe"))
            .args(["/D", "/C", &command])
            .creation_flags(0x0800_0000 | 0x0000_0008)
            .spawn();
    } else {
        let _ = std::fs::remove_dir_all(&install);
    }
    Ok(())
}
fn remove_empty_folders(folder: &Path) {
    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                remove_empty_folders(&entry.path());
            }
        }
    }
    let _ = std::fs::remove_dir(folder);
}
