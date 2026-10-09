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
/// Processes of Rubylight and of the hosts it replaces.
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
const SERVICE_DISPLAY_NAME: &str = "Rubylight";
const SERVICE_DESCRIPTION: &str = "Streams games and the desktop to Moonlight and Artemis clients.";
pub fn profile() -> PathBuf {
    system::program_data().join("Butterpollo").join("config")
}
/// The product was called Butterpollo before rc.30. Its firewall rule and
/// Start menu entry carry the name, so setup replaces the old ones; the
/// service, folders and file names keep theirs so upgrades stay in place.
pub const FIREWALL_RULE: &str = "Rubylight";
pub const LEGACY_FIREWALL_RULE: &str = "Butterpollo";
pub fn start_menu_link() -> PathBuf {
    system::program_data().join("Microsoft\\Windows\\Start Menu\\Programs\\Rubylight.lnk")
}
pub fn legacy_start_menu_link() -> PathBuf {
    system::program_data().join("Microsoft\\Windows\\Start Menu\\Programs\\Butterpollo.lnk")
}
/// After an in-place update: the firewall rule and Start menu entry under
/// the current name, replacing Butterpollo's once the new one exists.
pub fn refresh_entries(install: &Path, notes: &mut Vec<String>) {
    match system::firewall_allow(FIREWALL_RULE, &install.join("butterpollo.exe")) {
        Ok(()) => system::firewall_remove(LEGACY_FIREWALL_RULE),
        Err(error) => notes.push(format!(
            "The Windows Firewall rule could not be updated: {error:#}"
        )),
    }
    match system::shortcut(
        &start_menu_link(),
        &install.join("Start Butterpollo.exe"),
        "Open the Rubylight console",
    ) {
        Ok(()) => {
            let _ = std::fs::remove_file(legacy_start_menu_link());
        }
        Err(error) => notes.push(format!(
            "The Start menu shortcut could not be created: {error:#}"
        )),
    }
}
/// The service's name in Windows Services; its key stays `SERVICE`.
pub fn refresh_service_name() -> Result<()> {
    system::set_service_display_name(SERVICE, SERVICE_DISPLAY_NAME)
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
    found.check_version()?;
    line(format!("found: {found:#?}"));
    let install = system::win32_path(&detect::install_dir(&found, options.install_dir.clone()))?;
    let profile = profile();
    let mut notes = Vec::new();
    let mut restart_needed = false;

    if updates_in_place(found.service_install.as_deref(), &install)? {
        // As a reinstall did before, the service starts automatically again;
        // a disabled service would otherwise fail the update's start check.
        system::install_service(
            SERVICE,
            SERVICE_DISPLAY_NAME,
            SERVICE_DESCRIPTION,
            &install.join("butterpollo-service.exe"),
        )?;
        crate::update::run(&install, options.start, progress)?;
        // A reinstall also repairs the profile's permissions and the firewall
        // rule, as it did before.
        if let Err(error) = secure_profile(&profile) {
            notes.push(format!(
                "The permissions of {} could not be restricted: {error:#}",
                profile.display()
            ));
        }
        remember_driver_choice(&profile, options.display_driver);
        restart_needed |= install_drivers(
            &install,
            options.display_driver,
            options.gamepad_driver,
            progress,
            &mut notes,
        );
        refresh_entries(&install, &mut notes);
        return Ok(Outcome {
            web_port: web_port(&profile),
            install,
            restart_needed,
            notes,
        });
    }

    // Previous hosts are only removed by a full installation.
    let previous = migration_source(&found, &profile, &install)?;
    progress.set("Unpacking Rubylight…");
    let staging = system::program_data().join("Butterpollo").join("setup");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    // The staged host runs elevated below (settings import) and loads DLLs
    // from its own folder: lock the folder down before anything lands in it.
    system::restrict(&staging, false)?;
    payload.extract(&staging)?;
    let entries = payload::verify(&staging)?;

    let active_profile = previous
        .as_ref()
        .map(|root| {
            if root.join("config/sunshine.conf").is_file() {
                root.join("config")
            } else {
                root.clone()
            }
        })
        .unwrap_or_else(|| profile.clone());
    ensure_idle(probe(&active_profile))?;
    progress.set("Stopping the streaming host…");
    // Until the previous host is removed, a failure starts again exactly the
    // services that were running.
    let mut restart = stop_running(
        &OLD_SERVICES,
        system::service_running,
        system::stop_service,
        system::start_service,
    )?;
    system::kill(&HOST_PROCESSES);
    // An update that did not finish is rolled back first; its record would
    // otherwise make the service put that backup over this installation.
    crate::update::recover(&profile)?;

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

    if let Some(root) = &previous {
        progress.set("Importing settings, paired devices and apps…");
        let source = if root.join("config\\sunshine.conf").is_file() {
            root.join("config")
        } else {
            root.clone()
        };
        let (code, output) = system::run(
            &staging.join("butterpollo.exe").display().to_string(),
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

    progress.set("Installing files…");
    copy_package(&staging, &install, &entries)?;
    payload::write_stub(&install.join("uninstall.exe"))?;

    // The previous host is removed from here on, so a failure now starts
    // the service Rubylight runs as instead.
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

    progress.set("Registering the Rubylight service…");
    // Remaining old services would hold the streaming ports.
    for service in OLD_SERVICES.iter().filter(|s| **s != SERVICE) {
        let _ = system::delete_service(service);
    }
    system::install_service(
        SERVICE,
        SERVICE_DISPLAY_NAME,
        SERVICE_DESCRIPTION,
        &install.join("butterpollo-service.exe"),
    )?;
    system::firewall_allow(FIREWALL_RULE, &install.join("butterpollo.exe"))?;
    for rule in [
        LEGACY_FIREWALL_RULE,
        "Vibepollo",
        "Vibepollo Service",
        "Apollo",
    ] {
        system::firewall_remove(rule);
    }
    secure_profile(&profile)?;

    remember_driver_choice(&profile, options.display_driver);
    restart_needed |= install_drivers(
        &install,
        options.display_driver,
        options.gamepad_driver,
        progress,
        &mut notes,
    );

    progress.set("Adding Rubylight to Start and Apps…");
    match system::shortcut(
        &start_menu_link(),
        &install.join("Start Butterpollo.exe"),
        "Open the Rubylight console",
    ) {
        Ok(()) => {
            let _ = std::fs::remove_file(legacy_start_menu_link());
        }
        Err(error) => notes.push(format!(
            "The Start menu shortcut could not be created: {error:#}"
        )),
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
        progress.set("Starting Rubylight…");
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

/// Whether an installed Rubylight service is updated in place, with a
/// backup and rollback. An installation whose folder or manifest is gone has
/// nothing to restore; the full installation repairs it instead.
fn updates_in_place(service_install: Option<&Path>, install: &Path) -> Result<bool> {
    let Some(current) = service_install else {
        return Ok(false);
    };
    let Ok(current_folder) = current.canonicalize() else {
        line(format!(
            "the service folder {} is missing; installing in full",
            current.display()
        ));
        return Ok(false);
    };
    if install.canonicalize().ok().as_ref() != Some(&current_folder) {
        bail!(
            "Rubylight is installed in {}. Install the update into that folder so the previous version can be restored if needed. Nothing has been changed.",
            current.display()
        );
    }
    if let Err(error) = payload::manifest(&current_folder) {
        line(format!(
            "the installed package has no readable manifest ({error:#}); installing in full"
        ));
        return Ok(false);
    }
    Ok(true)
}
/// Canonicalizes the deepest existing ancestor and appends the rest, so a
/// folder that does not exist yet compares with canonical ones even when it
/// was given through an 8.3 short name (`C:\Users\RUNNER~1\...`).
fn canonical_prefix(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(canonical, |path, name| path.join(name));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => return path.to_owned(),
        }
    }
}
fn migration_source(
    found: &detect::Found,
    profile: &Path,
    install: &Path,
) -> Result<Option<PathBuf>> {
    let mut sources: Vec<(&str, PathBuf)> = Vec::new();
    for product in found
        .packages
        .iter()
        .chain(&found.legacy)
        .chain(&found.vibepollo_entries)
    {
        // An entry left behind after its folder was deleted has nothing to
        // import, and its removal deletes nothing.
        let Some(root) = product
            .location
            .as_ref()
            .and_then(|root| root.canonicalize().ok())
        else {
            line(format!(
                "{} has no installation folder; nothing to import",
                product.name
            ));
            continue;
        };
        let root = system::win32_path(&root)?;
        let root_path = root.to_string_lossy().to_lowercase();
        let install_path = system::win32_path(&canonical_prefix(install))?
            .to_string_lossy()
            .to_lowercase();
        if install_path == root_path || install_path.starts_with(&format!("{root_path}\\")) {
            bail!(
                "Choose an installation folder outside {}; removing the previous host could delete Rubylight's files",
                root.display()
            );
        }
        // The confirmation summary already says these settings were not
        // found; such a host is replaced without an import, as before.
        if !detect::has_profile(&root) {
            line(format!(
                "{} in {} has no settings to import",
                product.name,
                root.display()
            ));
        } else if !sources.iter().any(|(_, known)| known == &root) {
            sources.push((&product.name, root));
        }
    }
    let Some((name, source)) = sources.first() else {
        return Ok(None);
    };
    if sources.len() > 1 {
        let hosts = sources
            .iter()
            .map(|(name, root)| format!("{name} ({})", root.display()))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "Setup can import the settings of only one previous host, and these each have their own: {hosts}. Uninstall the ones whose settings you do not need, then run setup again. Nothing has been changed."
        );
    }
    let empty = match std::fs::read_dir(profile) {
        Ok(mut entries) => entries.next().transpose()?.is_none(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error).context("checking the existing Rubylight profile"),
    };
    if !empty {
        bail!(
            "Rubylight already has settings in {}, and {name} has its own in {}. To keep Rubylight's, uninstall {name} first; to import {name}'s instead, move the Rubylight folder elsewhere. Then run setup again. Nothing has been changed.",
            profile.display(),
            source.display()
        );
    }
    Ok(Some(source.clone()))
}
/// Stop `services`, restarting those that were running if any stop fails.
fn stop_running<F: FnMut(&str) -> Result<()>>(
    services: &[&'static str],
    running: impl Fn(&str) -> bool,
    mut stop: impl FnMut(&str) -> Result<()>,
    start: F,
) -> Result<Restart<F>> {
    let mut restart = Restart {
        services: Vec::new(),
        start,
    };
    for &service in services {
        if running(service) {
            restart.services.push(service);
        }
        stop(service)?;
    }
    Ok(restart)
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
    crate::update_files::replace(source, target, system::remove_file_later)
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
/// Set up the drivers with the scripts in `install`. Setup, an in-app update
/// and the service's repair take turns: two driver scripts at once would
/// stop and restart the service and the device under each other.
pub(crate) fn install_drivers(
    install: &Path,
    display: bool,
    gamepad: bool,
    progress: &Progress,
    notes: &mut Vec<String>,
) -> bool {
    let _turn = driver_lock(Duration::from_secs(900));
    run_drivers(install, display, gamepad, progress, notes)
}
fn run_drivers(
    install: &Path,
    display: bool,
    gamepad: bool,
    progress: &Progress,
    notes: &mut Vec<String>,
) -> bool {
    let mut restart_needed = false;
    if display && install.join("drivers\\display\\install.ps1").is_file() {
        progress.set("Installing the virtual display driver…");
        let script = install.join("drivers\\display\\install.ps1");
        restart_needed |=
            run_driver_script(&script, &["-InstallerBestEffort"], notes, "virtual display");
        // The host registers its own HDR Vulkan layer; Vibepollo's must not
        // be active at the same time.
        run_driver_script(
            &script,
            &["-UnregisterVulkanLayerOnly"],
            &mut Vec::new(),
            "Vulkan layer",
        );
    }
    if gamepad && install.join("drivers\\gamepad\\install.ps1").is_file() {
        progress.set("Installing the virtual gamepad driver…");
        restart_needed |= run_driver_script(
            &install.join("drivers\\gamepad\\install.ps1"),
            &["-InstallerBestEffort", "-AllowLocalTestCertificate:0"],
            notes,
            "virtual gamepad",
        );
    }

    restart_needed
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
    driver_result(code, &output, notes, name)
}
fn driver_result(code: i32, output: &str, notes: &mut Vec<String>, name: &str) -> bool {
    if output.contains("DRIVER_WARNING") || !matches!(code, 0 | 3010) {
        // The scripts name what failed: a blocked certificate or catalog, a
        // tool an antivirus stopped, a DriverStore that kept an old copy.
        let after = |marker: &str| {
            output
                .lines()
                .find_map(|line| line.split_once(marker))
                .map(|(_, reason)| reason.trim())
                .filter(|reason| !reason.is_empty())
        };
        let reason = after("driver action failed: ").or_else(|| after("DRIVER_WARNING: "));
        notes.push(match reason {
            Some(reason) => format!(
                "The {name} driver could not be set up: {reason} Rubylight tries again when its service starts; the setup log has the details."
            ),
            None => format!("The {name} driver reported a problem; see the setup log."),
        });
    }
    code == 3010 || output.contains("RESTART_REQUIRED") || output.contains("A reboot is required")
}
/// The one driver setup at a time, waited for up to `wait`. Without it the
/// driver scripts run anyway: a stale lock must not keep drivers out.
fn driver_lock(wait: Duration) -> Option<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    let profile = profile();
    let _ = std::fs::create_dir_all(&profile);
    let deadline = Instant::now() + wait;
    loop {
        match std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .share_mode(0)
            .open(profile.join("driver-setup.lock"))
        {
            Ok(file) => return Some(file),
            Err(error) if Instant::now() >= deadline => {
                line(format!("another setup is installing drivers ({error})"));
                return None;
            }
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}
const REPAIR_RECORD: &str = "driver-repair.json";
/// A new installation may repair the display driver again, unless it was
/// installed with --no-display-driver.
fn remember_driver_choice(profile: &Path, display_driver: bool) {
    let record = profile.join(REPAIR_RECORD);
    let _ = if display_driver {
        std::fs::remove_file(&record)
    } else {
        std::fs::create_dir_all(profile)
            .and_then(|()| std::fs::write(&record, br#"{"declined":true}"#))
    };
}
/// `butterpollo-setup.exe --repair-drivers`, which the service runs when its
/// host has had no virtual display driver for a minute. An old host's
/// uninstaller, an in-app update before rc.27 or a failed driver step leaves
/// it missing, and Artemis then reports the "SudoVDA" driver as not
/// installed. Sets the display driver up again from this installation, at
/// most once a day per version and never during a stream; the driver script
/// restarts the service. Returns whether Windows must restart.
pub fn repair_drivers(progress: &Progress) -> Result<bool> {
    let exe = std::env::current_exe()?;
    let install = system::win32_path(exe.parent().context("setup folder unavailable")?)?;
    let profile = profile();
    let record = profile.join(REPAIR_RECORD);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let version = env!("CARGO_PKG_VERSION");
    if let Some(reason) = repair_skipped(std::fs::read(&record).ok().as_deref(), version, now) {
        line(format!("not repairing the display driver: {reason}"));
        return Ok(false);
    }
    // Setup or an update that is installing the drivers right now restarts
    // the service when done, and the service checks again.
    let Some(_turn) = driver_lock(Duration::ZERO) else {
        return Ok(false);
    };
    ensure_idle(probe(&profile))?;
    std::fs::write(
        &record,
        serde_json::to_vec(&serde_json::json!({"version": version, "unix": now}))?,
    )?;
    let mut notes = Vec::new();
    let restart = run_drivers(&install, true, false, progress, &mut notes);
    for note in notes {
        line(format!("note: {note}"));
    }
    Ok(restart)
}
/// Whether setup ran with --no-display-driver.
pub(crate) fn display_driver_declined() -> bool {
    std::fs::read(profile().join(REPAIR_RECORD))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|record| record["declined"] == true)
}
fn repair_skipped(record: Option<&[u8]>, version: &str, now: u64) -> Option<&'static str> {
    let record: serde_json::Value = serde_json::from_slice(record?).ok()?;
    if record["declined"] == true {
        return Some("it was left out with --no-display-driver");
    }
    (record["version"] == version
        && record["unix"]
            .as_u64()
            .is_some_and(|at| now.saturating_sub(at) < 86_400))
    .then_some("this version already tried today")
}
fn remove_legacy(product: &crate::detect::Product) -> Result<()> {
    let (command, in_place) = legacy_uninstall(product, is_nsis).context("no uninstall command")?;
    let (code, _) = system::run_cmd(&command, Duration::from_secs(300))?;
    if code != 0 {
        bail!("its uninstaller exited with {code}");
    }
    // Run in place, an NSIS uninstaller cannot delete itself.
    if let Some(program) = in_place {
        let _ = std::fs::remove_file(&program);
        if let Some(folder) = program.parent() {
            let _ = std::fs::remove_dir(folder);
        }
    }
    Ok(())
}
/// The command that removes a legacy host, and the uninstaller it runs in
/// place. An NSIS uninstaller (Apollo, Sunshine, Vibepollo 1) copies itself
/// to %TEMP%, starts the copy and exits at once: setup went on while the
/// copy still had to stop and delete ApolloService, the service Rubylight
/// reuses, and remove drivers. With "_?=<folder>", always last and
/// unquoted, it runs in place and setup waits for it.
fn legacy_uninstall(
    product: &crate::detect::Product,
    nsis: impl Fn(&Path) -> bool,
) -> Option<(String, Option<PathBuf>)> {
    let command = product
        .quiet_uninstall
        .clone()
        .or_else(|| product.uninstall.clone().map(|u| format!("{u} /S")))?;
    let program = detect::program(&command).filter(|program| nsis(program));
    Some(match program {
        Some(program) if !command.contains("_?=") => {
            let folder = program.parent()?.display().to_string();
            (format!("{command} _?={folder}"), Some(program))
        }
        _ => (command, None),
    })
}
/// Whether a program is an NSIS installer or uninstaller: its data follows
/// the executable with this signature.
fn is_nsis(program: &Path) -> bool {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(program)
        .and_then(|file| file.take(16 << 20).read_to_end(&mut bytes))
        .is_ok()
        && bytes.windows(12).any(|window| window == b"NullsoftInst")
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
            ("DisplayName", Value::Text("Rubylight")),
            ("DisplayVersion", Value::Text(env!("CARGO_PKG_VERSION"))),
            ("Publisher", Value::Text("Rubylight")),
            ("InstallLocation", Value::Text(&location)),
            ("DisplayIcon", Value::Text(&icon)),
            ("UninstallString", Value::Text(&uninstall)),
            ("QuietUninstallString", Value::Text(&quiet)),
            (
                "URLInfoAbout",
                Value::Text("https://github.com/RamazanKara/Rubylight"),
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
/// Refuse to stop a host that reports a stream, a queued launch or a running
/// app. A host that cannot say is stopped as before: none is running (Windows
/// refuses a closed loopback port only after about two seconds), it is
/// Vibepollo, Apollo or Sunshine, or it was asked on a LAN bind_address,
/// where a Rust host leaves its counts blank.
pub(crate) fn ensure_idle(address: SocketAddr) -> Result<()> {
    match serverinfo(address) {
        Ok(response) => check_idle(&response),
        Err(error) => {
            line(format!("no host status at {address} ({error}); continuing"));
            Ok(())
        }
    }
}
fn serverinfo(address: SocketAddr) -> std::io::Result<String> {
    use std::io::{Read, Write};
    let mut socket = std::net::TcpStream::connect_timeout(&address, Duration::from_millis(500))?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
    write!(
        socket,
        "GET /serverinfo HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )?;
    // A reset or timeout after the answer arrived still leaves the counts.
    let mut response = Vec::new();
    let read = socket.take(65536).read_to_end(&mut response);
    if response.is_empty() {
        read?;
    }
    Ok(String::from_utf8_lossy(&response).into_owned())
}

fn check_idle(response: &str) -> Result<()> {
    for field in [
        "RustHostSessionCount",
        "RustHostPendingSessionCount",
        "RustHostApplicationActive",
    ] {
        let count = response
            .split_once(&format!("<{field}>"))
            .and_then(|(_, value)| value.split_once(&format!("</{field}>")))
            .and_then(|(value, _)| value.trim().parse::<u32>().ok());
        if count.is_some_and(|count| count != 0) {
            bail!(
                "Disconnect all streams and close games started from Moonlight before installing, then run setup again. Nothing has been changed."
            );
        }
    }
    Ok(())
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
    bail!("Rubylight did not start within 90 seconds; see logs\\service.log in the profile")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn driver_reboot_exit_codes_are_successful_but_warnings_are_shown() {
        let mut notes = Vec::new();
        assert!(driver_result(3010, "", &mut notes, "display"));
        assert!(notes.is_empty());
        assert!(!driver_result(0, "", &mut notes, "gamepad"));
        assert!(!driver_result(
            0,
            "VIRTUAL_DISPLAY_DRIVER_WARNING",
            &mut notes,
            "display"
        ));
        assert_eq!(notes.len(), 1);
        assert!(!driver_result(1, "failed", &mut notes, "gamepad"));
        assert_eq!(notes.len(), 2);
        // The script's own reason reaches the note.
        driver_result(
            0,
            "WARNING: [SunshineVirtualDisplay] VIRTUAL_DISPLAY_DRIVER_WARNING: Optional virtual display driver setup did not complete.\nWARNING: [SunshineVirtualDisplay] Installer best-effort driver action failed: nefconc.exe failed with exit code 5.\n",
            &mut notes,
            "virtual display",
        );
        assert!(
            notes[2].starts_with(
                "The virtual display driver could not be set up: nefconc.exe failed with exit code 5."
            ),
            "{}",
            notes[2]
        );
    }
    #[test]
    fn nsis_uninstallers_run_in_place_so_setup_waits_for_them() -> Result<()> {
        let product = |uninstall: Option<&str>, quiet: Option<&str>| detect::Product {
            key: "Apollo".into(),
            name: "Apollo".into(),
            version: "0.4.6".into(),
            location: None,
            uninstall: uninstall.map(Into::into),
            quiet_uninstall: quiet.map(Into::into),
            msi: false,
            root: HKEY_LOCAL_MACHINE,
        };
        let apollo = product(Some(r#""C:\Program Files\Apollo\Uninstall.exe""#), None);
        assert_eq!(
            legacy_uninstall(&apollo, |_| true),
            Some((
                r#""C:\Program Files\Apollo\Uninstall.exe" /S _?=C:\Program Files\Apollo"#.into(),
                Some(PathBuf::from(r"C:\Program Files\Apollo\Uninstall.exe"))
            ))
        );
        // Other uninstallers, and a command that already runs in place, are
        // run as registered.
        assert_eq!(
            legacy_uninstall(&apollo, |_| false),
            Some((r#""C:\Program Files\Apollo\Uninstall.exe" /S"#.into(), None))
        );
        let quiet = product(None, Some(r"C:\Apollo\Uninstall.exe /S _?=C:\Apollo"));
        assert_eq!(
            legacy_uninstall(&quiet, |_| true),
            Some((r"C:\Apollo\Uninstall.exe /S _?=C:\Apollo".into(), None))
        );
        assert_eq!(legacy_uninstall(&product(None, None), |_| true), None);

        let folder = tempfile::tempdir()?;
        let nsis = folder.path().join("Uninstall.exe");
        std::fs::write(
            &nsis,
            [&b"MZ"[..], &[0; 4096], b"\xef\xbe\xad\xdeNullsoftInst"].concat(),
        )?;
        let other = folder.path().join("unins000.exe");
        std::fs::write(&other, [&b"MZ"[..], &[0; 4096], b"Inno Setup"].concat())?;
        assert!(is_nsis(&nsis));
        assert!(!is_nsis(&other));
        assert!(!is_nsis(&folder.path().join("missing.exe")));
        Ok(())
    }
    #[test]
    fn the_display_driver_is_repaired_once_a_day_per_version_unless_declined() {
        let day = 86_400;
        let record = |text: &str| Some(text.as_bytes().to_vec());
        let skipped =
            |text: Option<Vec<u8>>, now| repair_skipped(text.as_deref(), "2.0.0-rc.27", now);
        assert_eq!(skipped(None, day), None);
        assert_eq!(skipped(record("not json"), day), None);
        let tried = record(r#"{"version":"2.0.0-rc.27","unix":86400}"#);
        assert!(skipped(tried.clone(), day + 60).is_some());
        assert_eq!(skipped(tried, 2 * day), None);
        assert_eq!(
            skipped(
                record(r#"{"version":"2.0.0-rc.26","unix":86400}"#),
                day + 60
            ),
            None
        );
        assert!(skipped(record(r#"{"declined":true}"#), day).is_some());
    }
    #[test]
    fn setup_refuses_streams_pending_connections_and_apps() {
        for counts in [[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]] {
            let response = format!(
                "<RustHostSessionCount>{}</RustHostSessionCount><RustHostPendingSessionCount>{}</RustHostPendingSessionCount><RustHostApplicationActive>{}</RustHostApplicationActive>",
                counts[0], counts[1], counts[2]
            );
            assert_eq!(check_idle(&response).is_ok(), counts == [0, 0, 0]);
        }
        // Older hosts, and a Rust host asked on its LAN bind_address, cannot
        // say; they are stopped as before rather than blocking setup forever.
        assert!(check_idle("<state>SUNSHINE_SERVER_FREE</state>").is_ok());
        assert!(check_idle("<RustHostVersion>2.0.0-rc.21</RustHostVersion><RustHostSessionCount></RustHostSessionCount><RustHostPendingSessionCount></RustHostPendingSessionCount><RustHostApplicationActive></RustHostApplicationActive>").is_ok());
        assert!(check_idle("").is_ok());
    }
    #[test]
    fn a_stopped_or_unresponsive_host_does_not_block_setup() -> Result<()> {
        // Windows refuses a closed loopback port only after about two seconds.
        let closed = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
        ensure_idle(closed)?;
        let silent = std::net::TcpListener::bind("127.0.0.1:0")?;
        ensure_idle(silent.local_addr()?)?;
        // A busy host is still refused over a real connection.
        let busy = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = busy.local_addr()?;
        let server = std::thread::spawn(move || -> std::io::Result<()> {
            use std::io::{Read, Write};
            let (mut socket, _) = busy.accept()?;
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte)?;
                request.push(byte[0]);
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\n\r\n<root><RustHostSessionCount>1</RustHostSessionCount></root>")
        });
        assert!(ensure_idle(address).is_err());
        server.join().unwrap()?;
        Ok(())
    }
    #[test]
    fn a_damaged_installation_is_repaired_by_the_full_installer() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let installed = temp.path().join("Butterpollo");
        std::fs::create_dir(&installed)?;
        assert!(!updates_in_place(None, &installed)?);
        // A deleted folder or a missing or broken manifest leaves nothing to
        // roll back to, so these do not block a reinstall.
        let deleted = temp.path().join("deleted");
        assert!(!updates_in_place(Some(&deleted), &deleted)?);
        assert!(!updates_in_place(Some(&installed), &installed)?);
        std::fs::write(installed.join("manifest.json"), "{")?;
        assert!(!updates_in_place(Some(&installed), &installed)?);
        std::fs::write(installed.join("manifest.json"), "[]")?;
        assert!(updates_in_place(Some(&installed), &installed)?);
        // Another folder would leave the service without its backup.
        let other = temp.path().join("other");
        assert!(updates_in_place(Some(&installed), &other).is_err());
        std::fs::create_dir(&other)?;
        assert!(updates_in_place(Some(&installed), &other).is_err());
        Ok(())
    }
    #[test]
    fn a_previous_host_is_removed_only_after_its_profile_can_be_imported() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let old = temp.path().join("Vibepollo");
        let profile = temp.path().join("Butterpollo/config");
        let install = temp.path().join("programs/Butterpollo");
        std::fs::create_dir_all(old.join("config"))?;
        std::fs::write(
            old.join("config/sunshine.conf"),
            "encoder=amdvce_experimental",
        )?;
        let product = detect::Product {
            key: "Vibepollo".into(),
            name: "Vibepollo".into(),
            version: "2.0.0".into(),
            location: Some(old.clone()),
            uninstall: None,
            quiet_uninstall: None,
            msi: false,
            root: HKEY_LOCAL_MACHINE,
        };
        let mut found = detect::Found {
            legacy: vec![product.clone()],
            ..Default::default()
        };
        let source = system::win32_path(&old.canonicalize()?)?;
        assert_eq!(migration_source(&found, &profile, &install)?, Some(source));
        assert!(migration_source(&found, &profile, &old).is_err());
        assert!(migration_source(&found, &profile, &old.join("nested")).is_err());
        std::fs::create_dir_all(&profile)?;
        std::fs::write(profile.join("sunshine_state.json"), "existing pairings")?;
        assert!(migration_source(&found, &profile, &install).is_err());
        assert_eq!(
            std::fs::read(profile.join("sunshine_state.json"))?,
            b"existing pairings"
        );
        std::fs::remove_file(profile.join("sunshine_state.json"))?;
        let other = temp.path().join("Sunshine");
        std::fs::create_dir(&other)?;
        std::fs::write(other.join("sunshine.conf"), "encoder=software")?;
        found.legacy.push(detect::Product {
            location: Some(other),
            ..product.clone()
        });
        assert!(migration_source(&found, &profile, &install).is_err());
        found.legacy.pop();
        // The same host listed twice (its MSI and its own entry) is one source.
        found.vibepollo_entries.push(product.clone());
        assert!(migration_source(&found, &profile, &install)?.is_some());
        found.vibepollo_entries.clear();
        // Hosts with nothing to import are replaced as the summary says, and
        // stale entries without a folder do not block setup.
        for location in [None, Some(temp.path().join("deleted"))] {
            found.legacy.push(detect::Product {
                location,
                ..product.clone()
            });
        }
        std::fs::remove_file(old.join("config/sunshine.conf"))?;
        assert_eq!(migration_source(&found, &profile, &install)?, None);
        // Even then Rubylight is not installed where a host is removed.
        assert!(migration_source(&found, &profile, &old.join("nested")).is_err());
        std::fs::write(profile.join("sunshine_state.json"), "existing pairings")?;
        assert_eq!(migration_source(&found, &profile, &install)?, None);
        assert_eq!(
            migration_source(&detect::Found::default(), &profile, &install)?,
            None
        );
        Ok(())
    }
    #[test]
    fn legacy_migrations_keep_complete_profiles_and_own_configured_files_and_covers() -> Result<()>
    {
        use crate::profile_tests::{assert_usable, package, rc23_profile, snapshot};
        use butterpollo_core::{config::Config, migration, state};
        use serde_json::{Value, json};
        use sha2::{Digest, Sha256};

        let temp = tempfile::tempdir()?;
        let template = temp.path().join("rc.23/config");
        rc23_profile(&template)?;
        let staged = temp.path().join("staged");
        let entries = package(&staged, env!("CARGO_PKG_VERSION"), "new.dll")?;
        for family in ["Vibepollo", "Apollo", "Sunshine"] {
            for external in [false, true] {
                let case = temp.path().join(format!("{family}-{external}"));
                let old = case.join(family);
                let source = if family == "Sunshine" {
                    old.clone()
                } else {
                    old.join("config")
                };
                let profile = case.join("Butterpollo/config");
                let install = case.join("programs/Butterpollo");
                copy_tree(&template, &source)?;
                let mut apps = state::load_json(&source.join("apps.json"), Value::Null)?;
                apps["apps"][1]["image-path"] = json!(source.join("covers/custom/Épopée.png"));
                state::write_json(&source.join("apps.json"), &apps)?;
                let mut config = Config::load(&source.join("sunshine.conf"))?;
                config.values.insert(
                    "log_path".into(),
                    source.join("butterpollo.log").display().to_string(),
                );
                let configured = [
                    ("file_state", "sunshine_state.json"),
                    ("file_apps", "apps.json"),
                    ("vibeshine_file_state", "vibeshine_state.json"),
                    ("credentials_file", "sunshine_credentials.json"),
                    ("cert", "credentials/cacert.pem"),
                    ("pkey", "credentials/cakey.pem"),
                ];
                if external {
                    let shared = old.join("shared files");
                    for (key, name) in configured {
                        let target = shared.join(name);
                        replace_file(&source.join(name), &target)?;
                        config
                            .values
                            .insert(key.into(), target.display().to_string());
                    }
                    let cover = shared.join("Épopée.png");
                    replace_file(&source.join("covers/custom/Épopée.png"), &cover)?;
                    apps["apps"][1]["image-path"] = json!(cover);
                    state::write_json(&shared.join("apps.json"), &apps)?;
                } else if family == "Sunshine" {
                    // Sunshine can keep the web password in the paired-state document.
                    std::fs::remove_file(source.join("sunshine_credentials.json"))?;
                }
                std::fs::write(source.join("sunshine.conf"), config.text())?;
                let source_before = snapshot(&old)?;
                let mut expected = snapshot(&source)?;
                expected.retain(|name, _| !name.starts_with("logs"));
                for (key, name) in configured {
                    if external {
                        expected
                            .insert(name.into(), std::fs::read(config.path(key, &source, name))?);
                        config.values.insert(key.into(), name.into());
                    }
                }
                config
                    .values
                    .insert("log_path".into(), "butterpollo.log".into());
                expected.insert("sunshine.conf".into(), config.text().into_bytes());
                let product = detect::Product {
                    key: family.into(),
                    name: family.into(),
                    version: "2.0.0".into(),
                    location: Some(old.clone()),
                    uninstall: None,
                    quiet_uninstall: None,
                    msi: false,
                    root: HKEY_LOCAL_MACHINE,
                };
                let found = detect::Found {
                    legacy: vec![product],
                    ..Default::default()
                };
                let selected = migration_source(&found, &profile, &install)?.unwrap();
                assert_eq!(selected, system::win32_path(&old.canonicalize()?)?);
                let selected_profile = if selected.join("config/sunshine.conf").is_file() {
                    selected.join("config")
                } else {
                    selected
                };
                // --import-config in the staged host calls this exact importer.
                migration::import(&selected_profile, &profile)?;
                copy_package(&staged, &install, &entries)?;
                for app in apps["apps"].as_array_mut().unwrap() {
                    let original = source.join(app["image-path"].as_str().unwrap());
                    let bytes = std::fs::read(original)?;
                    let name =
                        PathBuf::from("covers").join(format!("{:x}.png", Sha256::digest(&bytes)));
                    app["image-path"] = json!(profile.canonicalize()?.join(&name));
                    expected.insert(name, bytes);
                }
                expected.insert("apps.json".into(), serde_json::to_vec_pretty(&apps)?);
                assert_eq!(
                    snapshot(&profile)?,
                    expected,
                    "{family}, external={external}"
                );
                assert_eq!(snapshot(&old)?, source_before);
                assert_usable(&profile)?;
                assert!(migration_source(&found, &profile, &install).is_err());
                assert!(migration::import(&source, &profile).is_err());
                assert_eq!(snapshot(&profile)?, expected);
                assert_eq!(snapshot(&old)?, source_before);
                assert!(old.canonicalize()?.starts_with(temp.path().canonicalize()?));
                std::fs::remove_dir_all(&old)?;
                assert_usable(&profile)?;
                for app in apps["apps"].as_array().unwrap() {
                    let cover = PathBuf::from(app["image-path"].as_str().unwrap());
                    let relative = cover.strip_prefix(profile.canonicalize()?)?;
                    assert_eq!(std::fs::read(&cover)?, expected[relative]);
                }
                assert_eq!(snapshot(&profile)?, expected);
            }
        }
        Ok(())
    }
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
Error: the settings imported from C:\\old would keep Rubylight from starting\n\
\n\
Caused by:\n    0: invalid JSON in C:\\ProgramData\\Butterpollo\\.butterpollo-import-1\\vibeshine_state.json\n    1: expected value at line 1 column 1\n";
        assert_eq!(
            import_error(output).unwrap(),
            "the settings imported from C:\\old would keep Rubylight from starting\n\n\
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
    fn a_failed_stop_aborts_installation_and_restarts_the_previous_services() {
        let mut stopped = Vec::new();
        let mut started = Vec::new();
        let result = stop_running(
            &OLD_SERVICES,
            |_| true,
            |service| {
                stopped.push(service.to_owned());
                if service == "SunshineService" {
                    bail!("the service did not stop");
                }
                Ok(())
            },
            |service: &str| {
                started.push(service.to_owned());
                Ok(())
            },
        );
        assert!(result.is_err());
        drop(result);
        assert_eq!(stopped, ["ApolloService", "SunshineService"]);
        assert_eq!(started, stopped);
    }
    #[test]
    fn only_previously_running_services_restart_on_failure() -> Result<()> {
        let mut started = Vec::new();
        let restart = stop_running(
            &OLD_SERVICES,
            |service| service == "ApolloService",
            |_| Ok(()),
            |service: &str| {
                started.push(service.to_owned());
                Ok(())
            },
        )?;
        drop(restart);
        assert_eq!(started, ["ApolloService"]);
        let mut restart = Restart {
            services: vec!["ApolloService"],
            start: |_: &str| panic!("successful installation must not restart twice"),
        };
        restart.services.clear();
        drop(restart);
        Ok(())
    }
}
