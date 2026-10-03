//! What is installed: Butterpollo itself, Vibepollo-family MSI packages
//! (Vibepollo, Butterpollo C++, Apollo, Sunshine) and legacy NSIS installs.
use crate::system::{registry_dword, registry_string_view, service_program, subkeys};
use std::path::PathBuf;
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

fn read(root: HKEY, view: REG_SAM_FLAGS, key: &str) -> Option<Product> {
    let path = format!("{UNINSTALL}\\{key}");
    let name = registry_string_view(root, &path, "DisplayName", view)?;
    let location = registry_string_view(root, &path, "InstallLocation", view)
        .filter(|l| !l.trim().is_empty())
        .map(|l| PathBuf::from(l.trim().trim_end_matches('\\')));
    Some(Product {
        key: key.to_owned(),
        name: name.trim().to_owned(),
        version: registry_string_view(root, &path, "DisplayVersion", view).unwrap_or_default(),
        location,
        uninstall: registry_string_view(root, &path, "UninstallString", view),
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
    let location = found
        .vibepollo_entries
        .iter()
        .find_map(|p| p.location.clone())
        .or_else(|| {
            service_program(SERVICE)
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.eq_ignore_ascii_case("sunshinesvc.exe"))
                })
                .and_then(|p| p.parent()?.parent().map(PathBuf::from))
        });
    for package in &mut found.packages {
        if package.location.is_none() {
            package.location = location.clone();
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
        lines.push(format!(
            "{} {version} will be replaced. Its settings, paired devices, apps and drivers are kept.",
            product.name
        ));
    }
    lines.push(format!("Install folder: {}", install.display()));
    lines.join("\n")
}
