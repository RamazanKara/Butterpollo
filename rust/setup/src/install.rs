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
    net::{IpAddr, SocketAddr},
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
/// The text of a profile's sunshine.conf, empty if it cannot be read. The
/// host also reads one saved with a byte order mark or as UTF-16.
fn conf_text(profile: &Path) -> String {
    let bytes = std::fs::read(profile.join("sunshine.conf")).unwrap_or_default();
    let utf16 = |bytes: &[u8], unit: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| unit(pair))
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(b"\xff\xfe") {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xfe\xff") {
        return utf16(rest, u16::from_be_bytes);
    }
    let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    String::from_utf8_lossy(text).into_owned()
}
/// The configured base port of a profile (47989 unless set).
pub fn web_port(profile: &Path) -> u16 {
    let base = conf_text(profile)
        .lines()
        .find_map(|line| {
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
        .filter(|port| (1029..=65514).contains(port))
        .unwrap_or(47989);
    base + 1
}
/// Where the host answers serverinfo: its bind_address, or this PC when
/// that is blank, all interfaces or not an address (the host then listens
/// on all of them or only on this PC). The first value that is not empty
/// counts, as the host reads it.
fn probe_address(conf: &str) -> IpAddr {
    conf.lines()
        .find_map(|line| {
            let (key, value) = line.split('#').next()?.split_once('=')?;
            let value = value.trim().trim_matches('"').trim();
            (key.trim() == "bind_address" && !value.is_empty()).then_some(value)
        })
        .and_then(|value| value.parse::<IpAddr>().ok())
        .filter(|address| !address.is_unspecified())
        .unwrap_or(IpAddr::from([127, 0, 0, 1]))
}
/// The host's serverinfo address for a profile.
pub(crate) fn probe(profile: &Path) -> SocketAddr {
    SocketAddr::new(probe_address(&conf_text(profile)), web_port(profile) - 1)
}

pub fn install(options: &Options, progress: &Progress) -> Result<Outcome> {
    let mut payload = Payload::open()?
        .context("this setup.exe carries no package; build it with build.ps1 -Package")?;
    let found = detect::scan();
    line(format!("found: {found:#?}"));
    let install = system::win32_path(&detect::install_dir(&found, options.install_dir.clone()))?;
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
    // Until the previous host is removed, a failure starts again exactly the
    // services that were running.
    let mut restart = Restart {
        services: stop_running(&OLD_SERVICES, system::service_running, system::stop_service),
        start: system::start_service,
    };
    system::kill(&HOST_PROCESSES);
    // An update that did not finish is rolled back first; its record would
    // otherwise make the service put that backup over this installation.
    if let Err(error) = crate::update::recover(&profile) {
        line(format!("warning: {error:#}"));
    }

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
        let (code, output) = system::run(
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
            // Nothing has been removed yet: the services that were running
            // start again when this error returns.
            bail!(
                "importing the settings from {} failed:\n\n{}",
                source.display(),
                import_error(&output)
                    .unwrap_or_else(|| format!("butterpollo.exe exited with code {code}"))
            );
        }
        notes.push(format!(
            "Settings, paired devices and apps were imported from {}.",
            source.display()
        ));
    }

    // The previous host is removed from here on, so a failure now starts
    // the service Butterpollo runs as instead.
    restart.services = vec![SERVICE];
    for package in &found.packages {
        let Some(code) = package.product_code() else {
            continue;
        };
        progress.set(&format!("Removing {} {}…", package.name, package.version));
        let log = std::env::temp_dir().join("butterpollo-setup-previous-uninstall.log");
        let (result, _) = system::run(
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
    system::firewall_allow("Butterpollo", &install.join("butterpollo.exe"))?;
    for rule in ["Vibepollo", "Vibepollo Service", "Apollo"] {
        system::firewall_remove(rule);
    }
    secure_profile(&profile)?;

    if options.display_driver && install.join("drivers\\display\\install.ps1").is_file() {
        progress.set("Installing the virtual display driver…");
        let script = install.join("drivers\\display\\install.ps1");
        restart_needed |= run_driver_script(
            &script,
            &["-InstallerBestEffort"],
            &mut notes,
            "virtual display",
        );
        // The host registers its own HDR Vulkan layer; Vibepollo's must not
        // be active at the same time.
        run_driver_script(
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
        );
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
    // Earlier installers' entry, with Apollo's icon.
    let _ = std::fs::remove_file(
        system::program_data()
            .join("Microsoft\\Windows\\Start Menu\\Programs\\Butterpollo Rust.lnk"),
    );
    register(&install, &entries)?;

    let web_port = web_port(&profile);
    if options.start {
        progress.set("Starting Butterpollo…");
        system::start_service(SERVICE)?;
        wait_ready(probe(&profile), Some(env!("CARGO_PKG_VERSION")))?;
    }
    restart.services.clear();
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
/// Stop `services` and return those that were running before.
fn stop_running(
    services: &[&'static str],
    running: impl Fn(&str) -> bool,
    mut stop: impl FnMut(&str) -> Result<()>,
) -> Vec<&'static str> {
    let mut stopped = Vec::new();
    for &service in services {
        if running(service) {
            stopped.push(service);
        }
        if let Err(error) = stop(service) {
            line(format!("warning: {error:#}"));
        }
    }
    stopped
}
/// Starts `services` when dropped, so that setup returning early with an
/// error never leaves the streaming host stopped; cleared on success.
struct Restart<F: FnMut(&str) -> Result<()>> {
    services: Vec<&'static str>,
    start: F,
}
impl<F: FnMut(&str) -> Result<()>> Drop for Restart<F> {
    fn drop(&mut self) {
        for service in &self.services {
            if let Err(error) = (self.start)(service) {
                line(format!(
                    "the {service} service could not be restarted: {error:#}"
                ));
            }
        }
    }
}
/// What butterpollo.exe printed when it failed: anyhow's "Error: ..." and
/// its "Caused by:" chain, which follow any warnings.
fn import_error(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    let start = lines.iter().rposition(|line| line.starts_with("Error: "))?;
    let error = lines[start..].join("\n");
    Some(error["Error: ".len()..].trim().to_owned())
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
pub(crate) fn replace_file(source: &Path, target: &Path) -> Result<()> {
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
/// Run a Vibepollo driver script as SYSTEM. A failure is reported, not
/// fatal: the host still starts without the driver. Returns whether Windows
/// needs a restart.
fn run_driver_script(script: &Path, args: &[&str], notes: &mut Vec<String>, name: &str) -> bool {
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
    let result = system::run_as_system(
        &format!(
            "{}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
            std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into())
        ),
        &arguments,
        Duration::from_secs(600),
    );
    let (code, output) = match result {
        Ok(result) => result,
        Err(error) => {
            line(format!("warning: {error:#}"));
            notes.push(format!("The {name} driver could not be set up: {error:#}"));
            return false;
        }
    };
    if output.contains("DRIVER_WARNING") || code != 0 {
        notes.push(format!(
            "The {name} driver reported a problem; see the setup log."
        ));
    }
    output.contains("RESTART_REQUIRED") || output.contains("A reboot is required")
}
fn remove_legacy(product: &crate::detect::Product) -> Result<()> {
    let command = product
        .quiet_uninstall
        .clone()
        .or_else(|| product.uninstall.clone().map(|u| format!("{u} /S")))
        .context("no uninstall command")?;
    let (code, _) = system::run(
        &system::system32("cmd.exe"),
        &["/D", "/S", "/C", &format!("\"{command}\"")],
        Duration::from_secs(300),
    )?;
    if code != 0 {
        bail!("its uninstaller exited with {code}");
    }
    Ok(())
}
pub(crate) fn register(install: &Path, entries: &[payload::Entry]) -> Result<()> {
    // The updater uses canonical paths for file identity and backups. Do not
    // leak their verbatim prefix into the next manual install or shell entry.
    let install = system::win32_path(install)?;
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
/// Wait until the host answers serverinfo at `address` as a Rust host.
pub(crate) fn wait_ready(address: SocketAddr, version: Option<&str>) -> Result<()> {
    use std::io::{Read, Write};
    let host = match address {
        SocketAddr::V4(address) => address.ip().to_string(),
        SocketAddr::V6(address) => format!("[{}]", address.ip()),
    };
    let request = format!("GET /serverinfo HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if let Ok(mut socket) =
            std::net::TcpStream::connect_timeout(&address, Duration::from_millis(500))
        {
            let _ = socket.set_read_timeout(Some(Duration::from_secs(20)));
            let _ = socket.write_all(request.as_bytes());
            let mut response = String::new();
            let _ = socket.read_to_string(&mut response);
            if version.map_or_else(
                || response.contains("<RustHostVersion>"),
                |version| {
                    response.contains(&format!("<RustHostVersion>{version}</RustHostVersion>"))
                },
            ) {
                line("the host is answering");
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("Butterpollo did not start within 90 seconds; see logs\\service.log in the profile")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_health_check_asks_the_configured_bind_address() {
        let local = IpAddr::from([127, 0, 0, 1]);
        for (conf, expected) in [
            ("", local),
            ("port = 48000\n", local),
            ("bind_address =\n", local),
            ("bind_address = 0.0.0.0\n", local),
            ("bind_address = ::\n", local),
            ("bind_address = Ethernet\n", local),
            ("# bind_address = 192.168.1.50\n", local),
            (
                "bind_address = 192.168.1.50\n",
                IpAddr::from([192, 168, 1, 50]),
            ),
            (
                "bind_address = \"192.168.1.50\" # LAN only\r\n",
                IpAddr::from([192, 168, 1, 50]),
            ),
            (
                "bind_address =\nbind_address = 10.0.0.2\nbind_address = 10.0.0.3\n",
                IpAddr::from([10, 0, 0, 2]),
            ),
            ("bind_address = fd00::5\n", "fd00::5".parse().unwrap()),
        ] {
            assert_eq!(probe_address(conf), expected, "{conf:?}");
        }
        let profile = tempfile::tempdir().unwrap();
        let mut utf16 = vec![0xff, 0xfe];
        utf16.extend(
            "port = 48000\r\nbind_address = 192.168.1.50\r\n"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        std::fs::write(profile.path().join("sunshine.conf"), utf16).unwrap();
        assert_eq!(
            probe(profile.path()),
            SocketAddr::from(([192, 168, 1, 50], 48000))
        );
        std::fs::write(
            profile.path().join("sunshine.conf"),
            b"\xef\xbb\xbfport = 48000\n",
        )
        .unwrap();
        assert_eq!(
            probe(profile.path()),
            SocketAddr::from(([127, 0, 0, 1], 48000))
        );
        // The probe reaches a host listening only on that address.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let host = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 512];
            let length = socket.read(&mut request).unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\n\r\n<RustHostVersion>9.9</RustHostVersion>")
                .unwrap();
            String::from_utf8_lossy(&request[..length]).into_owned()
        });
        wait_ready(address, Some("9.9")).unwrap();
        assert!(host.join().unwrap().contains("Host: 127.0.0.1\r\n"));
    }
    #[test]
    fn the_import_error_and_its_causes_are_shown() {
        let output = "2026-10-07T10:00:00Z  WARN butterpollo_core::migration: profile file not imported file=C:\\old\\dump.bin reason=\"it is larger than 64 MiB\"\n\
Error: the settings imported from C:\\old would keep Butterpollo from starting\n\
\n\
Caused by:\n    0: invalid JSON in C:\\ProgramData\\Butterpollo\\.butterpollo-import-1\\vibeshine_state.json\n    1: expected value at line 1 column 1\n";
        assert_eq!(
            import_error(output).unwrap(),
            "the settings imported from C:\\old would keep Butterpollo from starting\n\n\
Caused by:\n    0: invalid JSON in C:\\ProgramData\\Butterpollo\\.butterpollo-import-1\\vibeshine_state.json\n    1: expected value at line 1 column 1"
        );
        assert_eq!(
            import_error("Error: no sunshine.conf\n").unwrap(),
            "no sunshine.conf"
        );
        assert!(import_error("thread 'main' panicked\n").is_none());
        assert!(import_error("").is_none());
    }
    #[test]
    fn every_service_that_was_running_starts_again() {
        let mut stopped = Vec::new();
        let running = stop_running(
            &OLD_SERVICES,
            |service| service != "ApolloService" && service != "ApolloSvc",
            |service| {
                stopped.push(service.to_owned());
                if service == "SunshineService" {
                    bail!("the {service} service did not stop");
                }
                Ok(())
            },
        );
        // Every old service is still asked to stop, as before.
        assert_eq!(stopped, OLD_SERVICES);
        assert_eq!(
            running,
            ["SunshineService", "VibeshineService", "sunshinesvc"]
        );
        let mut started = Vec::new();
        drop(Restart {
            services: running,
            start: |service: &str| {
                started.push(service.to_owned());
                if service == "SunshineService" {
                    bail!("still stopping");
                }
                Ok(())
            },
        });
        assert_eq!(
            started,
            ["SunshineService", "VibeshineService", "sunshinesvc"]
        );
        let mut started = Vec::new();
        let mut restart = Restart {
            services: vec!["SunshineService"],
            start: |service: &str| {
                started.push(service.to_owned());
                Ok(())
            },
        };
        restart.services.clear();
        drop(restart);
        assert!(started.is_empty());
    }
}
