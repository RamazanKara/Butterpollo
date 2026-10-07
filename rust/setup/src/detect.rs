//! What is installed: Butterpollo itself, Vibepollo-family MSI packages
//! (Vibepollo, Butterpollo C++, Apollo, Sunshine) and legacy NSIS installs.
use crate::system::{registry_dword, registry_string_view, service_program, subkeys};
use std::path::{Path, PathBuf};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_SAM_FLAGS,
};

pub const UNINSTALL: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
pub const SERVICE: &str = "ApolloService";
/// Services the previous hosts registered.
pub const OLD_SERVICES: [&str; 5] = [
    "ApolloService",
    "SunshineService",
    "VibeshineService",
    "sunshinesvc",
    "ApolloSvc",
];

#[derive(Clone, Debug)]
pub struct Product {
    /// The Uninstall subkey: a product code for Windows Installer packages.
    pub key: String,
    pub name: String,
    pub version: String,
    pub location: Option<PathBuf>,
    pub uninstall: Option<String>,
    pub quiet_uninstall: Option<String>,
    pub msi: bool,
    pub root: HKEY,
}
impl Product {
    pub fn product_code(&self) -> Option<&str> {
        (self.msi && self.key.starts_with('{') && self.key.ends_with('}')).then_some(&self.key)
    }
}
#[derive(Default, Debug)]
pub struct Found {
    /// Butterpollo's own entry.
    pub butterpollo: Option<Product>,
    /// Installed with Windows Installer: removed with msiexec.
    pub packages: Vec<Product>,
    /// Apollo or Sunshine installed by their own installers.
    pub legacy: Vec<Product>,
    /// Vibepollo's own Add/Remove Programs entry for its MSI.
    pub vibepollo_entries: Vec<Product>,
    /// The folder of a Rust host the service already runs, if any.
    pub service_install: Option<PathBuf>,
}
impl Found {
    pub fn check_version(&self) -> anyhow::Result<()> {
        if let Some(product) = &self.butterpollo {
            check_version(&product.version, env!("CARGO_PKG_VERSION"))?;
        }
        Ok(())
    }
    /// The Vibepollo-family installation whose settings to import.
    pub fn previous_root(&self) -> Option<PathBuf> {
        self.packages
            .iter()
            .chain(&self.vibepollo_entries)
            .chain(&self.legacy)
            .filter_map(|p| p.location.clone())
            .find(|root| root.join("config").join("sunshine.conf").is_file())
            .or_else(|| {
                // Older Apollo/Sunshine kept files beside the program.
                self.packages
                    .iter()
                    .chain(&self.legacy)
                    .filter_map(|p| p.location.clone())
                    .find(|root| root.join("sunshine.conf").is_file())
            })
    }
}

fn check_version(installed: &str, incoming: &str) -> anyhow::Result<()> {
    let release = installed
        .trim()
        .trim_start_matches(['v', 'V'])
        .split(['-', '+'])
        .next()
        .unwrap_or("");
    if release.split('.').count() < 3 || release.split('.').any(|part| part.parse::<u64>().is_err())
    {
        anyhow::bail!(
            "The installed Butterpollo version is unknown; restore its uninstall entry before upgrading"
        );
    }
    if crate::version::newer(installed, incoming) {
        anyhow::bail!(
            "Butterpollo {installed} is newer than this installer ({incoming}). Downgrades are not supported; your settings have not been changed."
        );
    }
    Ok(())
}

/// The program folder of an Uninstall entry: its InstallLocation, else the
/// folder of the program its DisplayIcon or UninstallString names. Some
/// installers leave InstallLocation out.
fn resolve_location(
    install_location: Option<&str>,
    display_icon: Option<&str>,
    uninstall: Option<&str>,
) -> Option<PathBuf> {
    if let Some(location) = install_location.map(str::trim).filter(|l| !l.is_empty()) {
        return Some(PathBuf::from(location.trim_end_matches('\\')));
    }
    [display_icon, uninstall]
        .into_iter()
        .flatten()
        .find_map(|text| program_folder(&program(text)?))
}
/// The program a DisplayIcon ("C:\App\app.exe,0") or a command line
/// ("\"C:\App\uninstall.exe\" /S") names, if it is a full path.
fn program(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    let path = match text.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next()?,
        None => match text.to_ascii_lowercase().find(".exe") {
            Some(end) => &text[..end + 4],
            None => text
                .rsplit_once(',')
                .filter(|(_, index)| index.trim().parse::<i32>().is_ok())
                .map_or(text, |(path, _)| path),
        },
    };
    let path = PathBuf::from(path.trim());
    path.is_absolute().then_some(path)
}
/// The folder a host's program is installed in (its service runs from
/// tools). Windows Installer's msiexec and icon cache belong to no host.
fn program_folder(program: &Path) -> Option<PathBuf> {
    if program
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("msiexec.exe"))
        || program
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("\\windows\\installer\\")
    {
        return None;
    }
    let folder = program.parent()?;
    if folder
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("tools"))
    {
        return folder.parent().map(Path::to_path_buf);
    }
    Some(folder.to_path_buf())
}
/// Whether a previous host's folder holds settings to import.
fn has_profile(root: &Path) -> bool {
    root.join("config").join("sunshine.conf").is_file() || root.join("sunshine.conf").is_file()
}

fn read(root: HKEY, view: REG_SAM_FLAGS, key: &str) -> Option<Product> {
    let path = format!("{UNINSTALL}\\{key}");
    let name = registry_string_view(root, &path, "DisplayName", view)?;
    let uninstall = registry_string_view(root, &path, "UninstallString", view);
    let location = resolve_location(
        registry_string_view(root, &path, "InstallLocation", view).as_deref(),
        registry_string_view(root, &path, "DisplayIcon", view).as_deref(),
        uninstall.as_deref(),
    );
    Some(Product {
        key: key.to_owned(),
        name: name.trim().to_owned(),
        version: registry_string_view(root, &path, "DisplayVersion", view).unwrap_or_default(),
        location,
        uninstall,
        quiet_uninstall: registry_string_view(root, &path, "QuietUninstallString", view),
        msi: registry_dword(root, &path, "WindowsInstaller", view) == Some(1),
        root,
    })
}

pub fn scan() -> Found {
    let mut found = Found::default();
    for (root, view) in [
        (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY),
        (HKEY_CURRENT_USER, KEY_WOW64_64KEY),
    ] {
        for key in subkeys(root, UNINSTALL, view) {
            let Some(product) = read(root, view, &key) else {
                continue;
            };
            let name = product.name.to_ascii_lowercase();
            if key.eq_ignore_ascii_case("Butterpollo") && root == HKEY_LOCAL_MACHINE {
                found.butterpollo = Some(product);
            } else if !matches!(
                name.as_str(),
                "vibepollo" | "vibeshine" | "apollo" | "sunshine"
            ) {
                continue;
            } else if product.product_code().is_some() {
                found.packages.push(product);
            } else if name == "vibepollo" || name == "vibeshine" {
                found.vibepollo_entries.push(product);
            } else if !found.legacy.iter().any(|p| p.name == product.name) {
                found.legacy.push(product);
            }
        }
    }
    // The hidden MSI entry has no location; take it from Vibepollo's entry.
    // Otherwise, as for any entry without one, the previous host's service
    // tells where it is installed.
    let service = OLD_SERVICES
        .iter()
        .filter_map(|name| service_program(name))
        .filter(|program| {
            program
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("sunshinesvc.exe"))
        })
        .find_map(|program| program_folder(&program));
    let location = found
        .vibepollo_entries
        .iter()
        .find_map(|p| p.location.clone())
        .or_else(|| service.clone());
    for package in &mut found.packages {
        if package.location.is_none() {
            package.location = location.clone();
        }
    }
    for product in found.vibepollo_entries.iter_mut().chain(&mut found.legacy) {
        if product.location.is_none() {
            product.location = service.clone();
        }
    }
    found.service_install = service_program(SERVICE)
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("butterpollo-service.exe"))
        })
        .and_then(|p| p.parent().map(PathBuf::from));
    found
}
/// Where Butterpollo is or will be installed.
pub fn install_dir(found: &Found, requested: Option<PathBuf>) -> PathBuf {
    requested
        .or_else(|| found.butterpollo.as_ref().and_then(|p| p.location.clone()))
        .or_else(|| found.service_install.clone())
        .unwrap_or_else(|| crate::system::program_files().join("Butterpollo"))
}
/// A short description for the confirmation dialog.
pub fn summary(found: &Found, install: &std::path::Path) -> String {
    let mut lines = Vec::new();
    if let Some(ours) = &found.butterpollo {
        lines.push(format!("Butterpollo {} will be updated.", ours.version));
    } else if found.service_install.is_some() {
        lines.push("The Butterpollo host already running as a service will be updated.".to_owned());
    }
    for product in found.packages.iter().chain(&found.legacy) {
        // Vibepollo's own entry carries the release version; its package
        // entry only the Windows Installer encoding (2.0.0-beta.3 -> 2.0.33.0).
        let version = found
            .vibepollo_entries
            .iter()
            .find(|entry| entry.location.is_some() && entry.location == product.location)
            .map_or(product.version.as_str(), |entry| entry.version.as_str());
        lines.push(if product.location.as_deref().is_some_and(has_profile) {
            format!(
                "{} {version} will be replaced. Its settings, paired devices, apps and drivers are kept.",
                product.name
            )
        } else {
            format!(
                "{} {version} will be replaced. Its settings were not found, so they cannot be kept.",
                product.name
            )
        });
    }
    lines.push(format!("Install folder: {}", install.display()));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upgrades_and_reinstalls_are_allowed_but_downgrades_are_not() {
        for (installed, incoming) in [
            ("2.0.0-rc.9", "2.0.0-rc.10"),
            ("2.0.0-rc.21", "2.0.0-rc.21"),
            ("2.0.0-rc.21", "2.0.0"),
        ] {
            assert!(check_version(installed, incoming).is_ok());
        }
        for (installed, incoming) in [
            ("2.0.0-rc.10", "2.0.0-rc.9"),
            ("2.0.0", "2.0.0-rc.21"),
            ("2.1.0", "2.0.0"),
            ("", "2.0.0-rc.21"),
            ("unknown", "2.0.0-rc.21"),
        ] {
            assert!(check_version(installed, incoming).is_err());
        }
    }
    #[test]
    fn locations_come_from_the_icon_the_uninstaller_or_the_service() {
        let path = |p: &str| Some(PathBuf::from(p));
        let sunshine = path(r"C:\Program Files\Sunshine");
        assert_eq!(
            resolve_location(Some(r" C:\Program Files\Sunshine\ "), None, None),
            sunshine
        );
        for (icon, uninstall) in [
            (Some(r"C:\Program Files\Sunshine\sunshine.exe,0"), None),
            (Some(r#""C:\Program Files\Sunshine\sunshine.exe",0"#), None),
            (Some(r"C:\Program Files\Sunshine\sunshine.ico"), None),
            (
                Some(""),
                Some(r#""C:\Program Files\Sunshine\uninstall.exe" /S"#),
            ),
            (None, Some(r"C:\Program Files\Sunshine\Uninstall.EXE /S")),
            (
                Some(r"C:\Windows\Installer\{0A1B}\ProductIcon.ico"),
                Some(r"C:\Program Files\Sunshine\tools\uninstall.exe"),
            ),
        ] {
            assert_eq!(
                resolve_location(Some("  "), icon, uninstall),
                sunshine,
                "{icon:?} {uninstall:?}"
            );
        }
        // Windows Installer's own programs and relative paths name no folder.
        assert_eq!(
            resolve_location(
                None,
                Some(r"C:\WINDOWS\Installer\{0A1B}\icon.ico,0"),
                Some(r"MsiExec.exe /X{0A1B}")
            ),
            None
        );
        assert_eq!(
            resolve_location(
                None,
                Some("sunshine.exe"),
                Some(r"C:\Windows\System32\msiexec.exe /X{0A1B}")
            ),
            None
        );
        assert_eq!(
            program_folder(Path::new(r"C:\Program Files\Apollo\tools\sunshinesvc.exe")),
            path(r"C:\Program Files\Apollo")
        );
    }
    #[test]
    fn the_summary_promises_settings_only_when_there_are_some() {
        let root = tempfile::tempdir().unwrap();
        let product = Product {
            key: "{0A1B}".into(),
            name: "Apollo".into(),
            version: "0.4.6".into(),
            location: Some(root.path().to_path_buf()),
            uninstall: None,
            quiet_uninstall: None,
            msi: true,
            root: HKEY_LOCAL_MACHINE,
        };
        let found = Found {
            packages: vec![product],
            ..Default::default()
        };
        let install = Path::new(r"C:\Program Files\Butterpollo");
        assert!(summary(&found, install).contains("Its settings were not found"));
        std::fs::create_dir(root.path().join("config")).unwrap();
        std::fs::write(root.path().join("config").join("sunshine.conf"), "").unwrap();
        assert!(
            summary(&found, install)
                .contains("Its settings, paired devices, apps and drivers are kept.")
        );
    }
}
