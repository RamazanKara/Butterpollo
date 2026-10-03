//! Installing, upgrading and replacing a previous Vibepollo-family host.
use crate::{
    detect::{self, OLD_SERVICES, SERVICE, UNINSTALL},
    log::line,
    payload::{self, Payload},
    system::{self, Value},
    ui::Progress,
};
use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

pub struct Options {
    pub install_dir: Option<PathBuf>,
    pub gamepad_driver: bool,
    pub display_driver: bool,
    pub start: bool,
}
pub struct Outcome {
    pub install: PathBuf,
    pub web_port: u16,
    pub restart_needed: bool,
    pub notes: Vec<String>,
}
/// Processes of Butterpollo and of the hosts it replaces.
pub const HOST_PROCESSES: [&str; 9] = [
    "butterpollo.exe",
    "butterpollo-service.exe",
    "Start Butterpollo.exe",
    "sunshine.exe",
    "sunshinesvc.exe",
    "sunshine_wgc_capture.exe",
    "sunshine_display_helper.exe",
    "playnite-launcher.exe",
    "playnite_launcher.exe",
];
pub fn profile() -> PathBuf {
    system::program_data().join("Butterpollo").join("config")
}
pub fn start_menu_link() -> PathBuf {
    system::program_data().join("Microsoft\\Windows\\Start Menu\\Programs\\Butterpollo.lnk")
}
/// The configured base port of a profile (47989 unless set).
pub fn web_port(profile: &Path) -> u16 {
    let base = std::fs::read_to_string(profile.join("sunshine.conf"))
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key.trim() == "port")
                    .then(|| {
                        value
                            .split('#')
                            .next()?
                            .trim()
                            .trim_matches('"')
                            .parse::<u16>()
                            .ok()
                    })
                    .flatten()
            })
        })
        .filter(|port| (1029..=65514).contains(port))
        .unwrap_or(47989);
    base + 1
}

pub fn install(options: &Options, progress: &Progress) -> Result<Outcome> {
    let mut payload = Payload::open()?
        .context("this setup.exe carries no package; build it with build.ps1 -Package")?;
    let found = detect::scan();
    line(format!("found: {found:#?}"));
    let install = detect::install_dir(&found, options.install_dir.clone());
    let profile = profile();
    let mut notes = Vec::new();
    let mut restart_needed = false;

    progress.set("Unpacking Butterpollo…");
    let staging = system::program_data().join("Butterpollo").join("setup");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    payload.extract(&staging)?;
    let entries = payload::verify(&staging)?;

    progress.set("Stopping the streaming host…");
    for service in OLD_SERVICES {
        if let Err(error) = system::stop_service(service) {
            line(format!("warning: {error:#}"));
        }
    }
    system::kill(&HOST_PROCESSES);

    let previous = found.previous_root();
    if let Some(root) = &previous {
        line(format!("previous installation: {}", root.display()));
        // Vibepollo journals NVIDIA profile changes; only its own program can
        // put them back, so do it before that program is removed.
        let undo = system::program_data().join("Sunshine\\nvprefs_undo.json");
        if undo.is_file() && root.join("sunshine.exe").is_file() {
            progress.set("Restoring NVIDIA settings changed by Vibepollo…");
            let _ = system::run(
                &root.join("sunshine.exe").display().to_string(),
                &["--restore-nvprefs-undo"],
                Duration::from_secs(60),
            );
        }
        // A package without drivers reuses the signed drivers already
        // installed with Vibepollo.
        for (from, to) in [
            ("drivers\\sunshine", "drivers\\display"),
            ("drivers\\vhf-gamepad", "drivers\\gamepad"),
        ] {
            if !staging.join(to).exists() && root.join(from).is_dir() {
                copy_tree(&root.join(from), &staging.join(to))?;
            }
        }
    }

    progress.set("Installing files…");
    copy_package(&staging, &install, &entries)?;
    payload::write_stub(&install.join("uninstall.exe"))?;

    if let Some(root) = &previous
        && profile_is_empty(&profile)
    {
        progress.set("Importing settings, paired devices and apps…");
        let source = if root.join("config\\sunshine.conf").is_file() {
            root.join("config")
        } else {
            root.clone()
        };
        let code = system::run(
            &install.join("butterpollo.exe").display().to_string(),
            &[
                "--config-dir",
                &profile.display().to_string(),
                "--import-config",
                &source.display().to_string(),
            ],
            Duration::from_secs(300),
        )?;
        if code != 0 {
            // Nothing has been removed yet; the previous host stays usable.
            bail!(
                "importing the settings from {} failed; see the log",
                source.display()
            );
        }
        notes.push(format!(
            "Settings, paired devices and apps were imported from {}.",
            source.display()
        ));
    }

    for package in &found.packages {
        let Some(code) = package.product_code() else {
            continue;
        };
        progress.set(&format!("Removing {} {}…", package.name, package.version));
        let log = std::env::temp_dir().join("butterpollo-setup-previous-uninstall.log");
        let result = system::run(
            "msiexec.exe",
            &[
                "/x",
                code,
                "/qn",
                "/norestart",
                "/l*v",
                &log.display().to_string(),
                "REBOOT=ReallySuppress",
                "SUPPRESSMSGBOXES=1",
                "FACTORYRESET=0",
                "REMOVEVIRTUALDISPLAYDRIVER=0",
                "REMOVEVIRTUALGAMEPADDRIVER=0",
            ],
            Duration::from_secs(900),
        )?;
        match result {
            0 | 1605 => {}
            3010 => restart_needed = true,
            code => notes.push(format!(
                "{} could not be removed completely (msiexec {code}); remove it from Settings > Apps.",
                package.name
            )),
        }
    }
    for entry in &found.vibepollo_entries {
        // The Vibepollo uninstaller's own entry, if the package left it.
        if system::registry_string(
            entry.root,
            &format!("{UNINSTALL}\\{}", entry.key),
            "DisplayName",
        )
        .is_some()
            && found.packages.iter().any(|p| p.product_code().is_some())
        {
            system::delete_key(entry.root, &format!("{UNINSTALL}\\{}", entry.key));
        }
    }
    for legacy in &found.legacy {
        progress.set(&format!("Removing {} {}…", legacy.name, legacy.version));
        if let Err(error) = remove_legacy(legacy) {
            notes.push(format!("{} could not be removed: {error:#}", legacy.name));
        }
    }

    progress.set("Registering the Butterpollo service…");
    // Remaining old services would hold the streaming ports.
    for service in OLD_SERVICES.iter().filter(|s| **s != SERVICE) {
        let _ = system::delete_service(service);
    }
    system::install_service(
        SERVICE,
        "Butterpollo",
        "Streams games and the desktop to Moonlight and Artemis clients.",
        &install.join("butterpollo-service.exe"),
    )?;
    for rule in ["Vibepollo", "Vibepollo Service", "Apollo"] {
        system::firewall_remove(rule);
    }
    system::firewall_allow("Butterpollo", &install.join("butterpollo.exe"))?;
    secure_profile(&profile)?;

    if options.display_driver && install.join("drivers\\display\\install.ps1").is_file() {
        progress.set("Installing the virtual display driver…");
        let script = install.join("drivers\\display\\install.ps1");
        restart_needed |= run_driver_script(
            &script,
            &["-InstallerBestEffort"],
            &mut notes,
            "virtual display",
        )?;
        // The host registers its own HDR Vulkan layer; Vibepollo's must not
        // be active at the same time.
        let _ = run_driver_script(
            &script,
            &["-UnregisterVulkanLayerOnly"],
            &mut Vec::new(),
            "Vulkan layer",
        );
    }
    if options.gamepad_driver && install.join("drivers\\gamepad\\install.ps1").is_file() {
        progress.set("Installing the virtual gamepad driver…");
        restart_needed |= run_driver_script(
            &install.join("drivers\\gamepad\\install.ps1"),
            &["-InstallerBestEffort", "-AllowLocalTestCertificate:0"],
            &mut notes,
            "virtual gamepad",
        )?;
    }

    progress.set("Adding Butterpollo to Start and Apps…");
    if let Err(error) = system::shortcut(
        &start_menu_link(),
        &install.join("Start Butterpollo.exe"),
        "Open the Butterpollo console",
    ) {
        notes.push(format!(
            "The Start menu shortcut could not be created: {error:#}"
        ));
    }
    let _ = std::fs::remove_dir_all(
        system::program_data().join("Microsoft\\Windows\\Start Menu\\Programs\\Vibepollo"),
    );
    register(&install, &entries)?;

    let web_port = web_port(&profile);
    if options.start {
        progress.set("Starting Butterpollo…");
        system::start_service(SERVICE)?;
        wait_ready(web_port - 1)?;
    }
    let _ = std::fs::remove_dir_all(&staging);
    Ok(Outcome {
        install,
        web_port,
        restart_needed,
        notes,
    })
}

fn profile_is_empty(profile: &Path) -> bool {
    std::fs::read_dir(profile).map_or(true, |mut entries| entries.next().is_none())
}
/// SYSTEM and Administrators control the service profile, users may read
/// it (the launcher reads the port); credentials are not readable by users.
fn secure_profile(profile: &Path) -> Result<()> {
    let root = profile.parent().context("profile has no parent")?;
    std::fs::create_dir_all(profile.join("credentials"))?;
    system::restrict(root, true)?;
    system::inherit_contents(root)?;
    system::restrict(&profile.join("credentials"), false)
}

/// Copy the package, replacing files still loaded by other programs (the
/// Vulkan layer inside a running game) by renaming them first.
fn copy_package(staging: &Path, install: &Path, entries: &[payload::Entry]) -> Result<()> {
    std::fs::create_dir_all(install)?;
    if let Ok(previous) = payload::manifest(install) {
        for old in previous {
            if !entries
                .iter()
                .any(|e| e.path.eq_ignore_ascii_case(&old.path))
                && let Ok(path) = payload::safe_join(install, &old.path)
            {
                system::remove_file_later(&path);
            }
        }
    }
    for entry in entries {
        let source = payload::safe_join(staging, &entry.path)?;
        let target = payload::safe_join(install, &entry.path)?;
        replace_file(&source, &target)?;
    }
    replace_file(
        &staging.join("manifest.json"),
        &install.join("manifest.json"),
    )?;
    // Drivers reused from a previous installation are not in the manifest.
    let drivers = staging.join("drivers");
    if drivers.is_dir() {
        copy_tree(&drivers, &install.join("drivers"))?;
    }
    Ok(())
}
fn replace_file(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::copy(source, target).is_ok() {
        return Ok(());
    }
    // In use: move it aside, delete it at restart and copy the new file.
    let aside = target.with_extension(format!(
        "{}.old-{}",
        target.extension().and_then(|e| e.to_str()).unwrap_or(""),
        std::process::id()
    ));
    std::fs::rename(target, &aside).with_context(|| format!("replacing {}", target.display()))?;
    system::remove_file_later(&aside);
    std::fs::copy(source, target).with_context(|| format!("installing {}", target.display()))?;
    Ok(())
}
fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let destination = target.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else if kind.is_file() {
            replace_file(&entry.path(), &destination)?;
        }
    }
    Ok(())
}
/// Run a Vibepollo driver script. Its best-effort mode never fails; its
/// output says when Windows needs a restart.
fn run_driver_script(
    script: &Path,
    args: &[&str],
    notes: &mut Vec<String>,
    name: &str,
) -> Result<bool> {
    let script = script.display().to_string();
    let mut arguments = vec![
        "-NoLogo",
        "-NonInteractive",
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        script.as_str(),
    ];
    arguments.extend_from_slice(args);
    let (code, output) = system::run_output(
        &format!(
            "{}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
            std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into())
        ),
        &arguments,
        Duration::from_secs(600),
    )?;
    if output.contains("DRIVER_WARNING") || code != 0 {
        notes.push(format!(
            "The {name} driver reported a problem; see the setup log."
        ));
    }
    Ok(output.contains("RESTART_REQUIRED") || output.contains("A reboot is required"))
}
fn remove_legacy(product: &crate::detect::Product) -> Result<()> {
    let command = product
        .quiet_uninstall
        .clone()
        .or_else(|| product.uninstall.clone().map(|u| format!("{u} /S")))
        .context("no uninstall command")?;
    let code = system::run(
        &system::system32("cmd.exe"),
        &["/D", "/S", "/C", &format!("\"{command}\"")],
        Duration::from_secs(300),
    )?;
    if code != 0 {
        bail!("its uninstaller exited with {code}");
    }
    Ok(())
}
fn register(install: &Path, entries: &[payload::Entry]) -> Result<()> {
    let size_kb: u64 = entries
        .iter()
        .filter_map(|e| std::fs::metadata(install.join(&e.path)).ok())
        .map(|m| m.len())
        .sum::<u64>()
        / 1024;
    let location = format!("{}\\", install.display());
    let uninstaller = install.join("uninstall.exe").display().to_string();
    let uninstall = format!("\"{uninstaller}\" --uninstall");
    let quiet = format!("\"{uninstaller}\" --uninstall --quiet");
    let icon = install.join("butterpollo.exe").display().to_string();
    system::write_key(
        HKEY_LOCAL_MACHINE,
        &format!("{UNINSTALL}\\Butterpollo"),
        &[
            ("DisplayName", Value::Text("Butterpollo")),
            ("DisplayVersion", Value::Text(env!("CARGO_PKG_VERSION"))),
            ("Publisher", Value::Text("Butterpollo")),
            ("InstallLocation", Value::Text(&location)),
            ("DisplayIcon", Value::Text(&icon)),
            ("UninstallString", Value::Text(&uninstall)),
            ("QuietUninstallString", Value::Text(&quiet)),
            (
                "URLInfoAbout",
                Value::Text("https://github.com/RamazanKara/Butterpollo"),
            ),
            ("NoModify", Value::Number(1)),
            ("NoRepair", Value::Number(1)),
            (
                "EstimatedSize",
                Value::Number(size_kb.min(u64::from(u32::MAX)) as u32),
            ),
        ],
    )
}
/// Wait until the host answers serverinfo as a Rust host.
fn wait_ready(port: u16) -> Result<()> {
    use std::io::{Read, Write};
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if let Ok(mut socket) = std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_millis(500),
        ) {
            let _ = socket.set_read_timeout(Some(Duration::from_secs(20)));
            let _ = socket.write_all(
                b"GET /serverinfo HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            );
            let mut response = String::new();
            let _ = socket.read_to_string(&mut response);
            if response.contains("<RustHostVersion>") {
                line("the host is answering");
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("Butterpollo did not start within 90 seconds; see logs\\service.log in the profile")
}
