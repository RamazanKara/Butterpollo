//! Copy an existing Windows profile into an empty, independent Rust profile.
use crate::{catalog, config::Config, crypto, state};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}
const FILE_LIMIT: u64 = 64 * 1024 * 1024;
/// Copy a file the host needs. Leaving it out would lose paired devices,
/// apps or credentials, so a link or an oversized file fails the import.
fn copy_file(source: &Path, destination: &Path, files: &mut usize, total: &mut u64) -> Result<()> {
    let metadata = source.symlink_metadata()?;
    if linked(&metadata) {
        bail!("profile file is a link: {}", source.display());
    }
    if metadata.len() > FILE_LIMIT {
        bail!("profile file is larger than 64 MiB: {}", source.display());
    }
    *files += 1;
    *total = total.saturating_add(metadata.len());
    std::fs::create_dir_all(
        destination
            .parent()
            .context("profile file needs a directory")?,
    )?;
    std::fs::copy(source, destination)?;
    Ok(())
}
/// Copy a file the host can do without. A link, a file over 64 MiB, or one
/// past 10,000 files or 512 MiB in all is left out with a warning instead of
/// failing the import. Returns whether the file was copied.
fn copy_optional(
    source: &Path,
    destination: &Path,
    files: &mut usize,
    total: &mut u64,
) -> Result<bool> {
    let metadata = source.symlink_metadata()?;
    let reason = if linked(&metadata) {
        "it is a link"
    } else if metadata.len() > FILE_LIMIT {
        "it is larger than 64 MiB"
    } else if *files >= 10000 || total.saturating_add(metadata.len()) > 512 * 1024 * 1024 {
        "the import is limited to 10,000 files and 512 MiB"
    } else {
        copy_file(source, destination, files, total)?;
        return Ok(true);
    };
    tracing::warn!(file = %source.display(), reason, "profile file not imported");
    Ok(false)
}
pub fn profile_id(directory: &Path) -> String {
    let canonical = directory
        .canonicalize()
        .unwrap_or_else(|_| directory.to_path_buf());
    hex::encode(Sha256::digest(
        canonical.to_string_lossy().to_lowercase().as_bytes(),
    ))
}
/// Copy a profile folder. The previous host's logs folder stays behind: its
/// session logs can fill hundreds of MiB, and the new host writes its own.
fn copy_tree(
    source: &Path,
    destination: &Path,
    files: &mut usize,
    total: &mut u64,
    root: bool,
) -> Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let metadata = entry.path().symlink_metadata()?;
        if linked(&metadata) || metadata.is_file() {
            copy_optional(
                &entry.path(),
                &destination.join(entry.file_name()),
                files,
                total,
            )?;
        } else if metadata.is_dir() && !(root && entry.file_name().eq_ignore_ascii_case("logs")) {
            copy_tree(
                &entry.path(),
                &destination.join(entry.file_name()),
                files,
                total,
                false,
            )?;
        }
    }
    Ok(())
}
pub fn import(source: &Path, destination: &Path) -> Result<()> {
    let source = source
        .canonicalize()
        .context("existing profile folder unavailable")?;
    let config = Config::load(&source.join("sunshine.conf"))?;
    if !source.join("sunshine.conf").is_file() {
        bail!("select the folder containing sunshine.conf");
    }
    // Validate familiar documents before committing an import; unknown fields stay.
    state::PairedState::load(&config.path("file_state", &source, "sunshine_state.json"))?;
    state::load_json(
        &config.path("file_apps", &source, "apps.json"),
        serde_json::json!({}),
    )?;
    if destination.exists() && std::fs::read_dir(destination)?.next().is_some() {
        bail!("destination already contains a profile");
    }
    let parent = destination
        .parent()
        .context("profile needs a parent directory")?;
    std::fs::create_dir_all(parent)?;
    let destination = parent.canonicalize()?.join(
        destination
            .file_name()
            .context("invalid profile directory")?,
    );
    if destination.starts_with(&source) || source.starts_with(&destination) {
        bail!("profiles must be in independent folders");
    }
    let stage = destination.parent().unwrap().join(format!(
        ".butterpollo-import-{}",
        hex::encode(crate::crypto::random::<16>())
    ));
    let result = (|| -> Result<()> {
        let (mut files, mut total) = (0, 0);
        copy_tree(&source, &stage, &mut files, &mut total, true)?;
        let mut rewritten = config.clone();
        // The certificate and key only work as a pair. When one is missing
        // the C++ host makes a new pair; clearing the default place lets this
        // host do the same instead of refusing to start.
        let identity = [
            config.path("cert", &source, "credentials/cacert.pem"),
            config.path("pkey", &source, "credentials/cakey.pem"),
        ];
        let identity_complete = identity.iter().all(|path| path.is_file());
        if !identity_complete {
            if identity.iter().any(|path| path.is_file())
                || config.values.contains_key("cert")
                || config.values.contains_key("pkey")
            {
                tracing::warn!(
                    certificate = %identity[0].display(),
                    key = %identity[1].display(),
                    "the certificate or its key is missing; the host makes a new pair"
                );
            }
            for target in ["credentials/cacert.pem", "credentials/cakey.pem"] {
                if stage.join(target).is_file() {
                    std::fs::remove_file(stage.join(target))?;
                }
            }
        }
        for (key, target) in [
            ("file_state", "sunshine_state.json"),
            ("file_apps", "apps.json"),
            ("vibeshine_file_state", "vibeshine_state.json"),
            ("credentials_file", "sunshine_credentials.json"),
            ("cert", "credentials/cacert.pem"),
            ("pkey", "credentials/cakey.pem"),
        ] {
            let configured = config.values.contains_key(key);
            let original = config.path(key, &source, target);
            let usable = if matches!(key, "cert" | "pkey") {
                identity_complete
            } else {
                original.is_file()
            };
            if usable && (configured || !stage.join(target).is_file()) {
                // The folder copy leaves out what is past its limits; these
                // files are needed, so they are copied in any case.
                let path = stage.join(target);
                copy_file(&original, &path, &mut files, &mut total)?;
                if configured {
                    rewritten.values.insert(key.into(), target.into());
                }
            } else if usable || !configured {
                continue;
            } else if key == "vibeshine_file_state" {
                rewritten.values.insert(key.into(), target.into());
            } else {
                // As in the C++ host, the default file is used (or made).
                if !matches!(key, "cert" | "pkey") {
                    tracing::warn!(
                        key,
                        file = %original.display(),
                        "a configured profile file is missing; the default is used"
                    );
                }
                rewritten.values.remove(key);
            }
        }
        // Own existing PNG covers too, so uninstalling the original host does
        // not remove the migrated library's artwork. Commands remain intact.
        let apps_path = rewritten.path("file_apps", &stage, "apps.json");
        if apps_path.is_file() {
            let mut apps = state::load_json(&apps_path, serde_json::json!({}))?;
            let mut changed = false;
            if let Some(entries) = apps
                .get_mut("apps")
                .and_then(serde_json::Value::as_array_mut)
            {
                for app in entries {
                    let Some(image) = app.get("image-path").and_then(serde_json::Value::as_str)
                    else {
                        continue;
                    };
                    let image = PathBuf::from(image);
                    let original = if image.is_absolute() {
                        image
                    } else {
                        source.join(image)
                    };
                    if !original.is_file()
                        || !original
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
                    {
                        continue;
                    }
                    // A cover that is not copied keeps its path; the app
                    // shows the default artwork if the host cannot use it.
                    let metadata = original.symlink_metadata()?;
                    if linked(&metadata) || metadata.len() > 16 * 1024 * 1024 {
                        tracing::warn!(
                            cover = %original.display(),
                            "cover not imported: it is a link or larger than 16 MiB"
                        );
                        continue;
                    }
                    let bytes = std::fs::read(&original)?;
                    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                        continue;
                    }
                    let target = PathBuf::from("covers")
                        .join(format!("{}.png", hex::encode(Sha256::digest(&bytes))));
                    if !stage.join(&target).is_file()
                        && !copy_optional(&original, &stage.join(&target), &mut files, &mut total)?
                    {
                        continue;
                    }
                    app["image-path"] =
                        serde_json::json!(destination.join(&target).to_string_lossy());
                    changed = true;
                }
            }
            if changed {
                state::atomic_write(&apps_path, &serde_json::to_vec_pretty(&apps)?)?;
            }
        }
        // Log output must also stay in the newly owned profile.
        if config.values.contains_key("log_path") {
            rewritten
                .values
                .insert("log_path".into(), "butterpollo.log".into());
        }
        state::atomic_write(&stage.join("sunshine.conf"), rewritten.text().as_bytes())?;
        check(&stage).context("the imported settings would keep Butterpollo from starting")?;
        if destination.exists() {
            std::fs::remove_dir(&destination)?;
        }
        std::fs::rename(&stage, &destination)?;
        Ok(())
    })();
    if result.is_err() && stage.exists() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    result
}
/// Read a profile as the host does when it starts, without writing to it,
/// so that a file the host would refuse fails the import instead.
fn check(directory: &Path) -> Result<()> {
    let config = Config::load(&directory.join("sunshine.conf"))?;
    let files = state::ProfileFiles::new(&config, directory);
    state::PairedState::load(&files.paired)?;
    crypto::Identity::read(&files.certificate, &files.key)?;
    state::Credentials::load(&files.credentials)?;
    let mut aliases = state::load_json(&files.aliases, json!({"root":{}}))?;
    // The host sets aside an app library it cannot read and starts without
    // it, and gives an app without a UUID one, before it checks the app IDs
    // kept with the aliases.
    let mut apps: Vec<state::App> = state::load_json(&files.apps, json!({}))
        .ok()
        .and_then(|library| {
            serde_json::from_value(library.get("apps").cloned().unwrap_or(json!([]))).ok()
        })
        .unwrap_or_default();
    for app in &mut apps {
        if app
            .extra
            .get("uuid")
            .and_then(Value::as_str)
            .is_none_or(|uuid| uuid.trim().is_empty())
        {
            app.extra
                .insert("uuid".into(), json!(uuid::Uuid::new_v4().to_string()));
        }
    }
    catalog::assign(&mut apps, &mut aliases, directory)
}
pub fn default_directory() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?)
            .join("ButterpolloRust/config"),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn importing_keeps_source_intact_unknown_fields_and_external_files_owned() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old");
        std::fs::create_dir(&old).unwrap();
        let apps = temp.path().join("apps.json");
        let original = br#"{"env":{"CUSTOM":"yes"},"apps":[],"future":{"keep":true}}"#;
        std::fs::write(&apps, original).unwrap();
        let conf = format!(
            "file_apps={}\nlog_path={}\nfuture_setting=unchanged\n",
            apps.display(),
            temp.path().join("old.log").display()
        );
        std::fs::write(old.join("sunshine.conf"), &conf).unwrap();
        let next = temp.path().join("rust");
        import(&old, &next).unwrap();
        assert_eq!(
            std::fs::read(old.join("sunshine.conf")).unwrap(),
            conf.as_bytes()
        );
        assert_eq!(std::fs::read(&apps).unwrap(), original);
        assert_eq!(std::fs::read(next.join("apps.json")).unwrap(), original);
        let config = Config::load(&next.join("sunshine.conf")).unwrap();
        assert_eq!(config.get("file_apps", ""), "apps.json");
        assert_eq!(config.get("future_setting", ""), "unchanged");
        assert!(import(&old, &next).is_err());
        assert!(import(&old, &old.join("inside")).is_err());
    }
    #[test]
    fn configured_files_that_are_missing_fall_back_to_the_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old");
        std::fs::create_dir_all(old.join("credentials")).unwrap();
        std::fs::write(old.join("apps.json"), br#"{"apps":[{"name":"Kept"}]}"#).unwrap();
        // The certificate is in its default place, its configured key is not.
        std::fs::write(old.join("credentials/cacert.pem"), "certificate").unwrap();
        let gone = temp.path().join("gone");
        std::fs::write(
            old.join("sunshine.conf"),
            format!(
                "file_state={0}\\state.json\nfile_apps={0}\\apps.json\ncredentials_file={0}\\credentials.json\ncert=credentials\\cacert.pem\npkey={0}\\cakey.pem\nvibeshine_file_state={0}\\vibeshine.json\n",
                gone.display()
            ),
        )
        .unwrap();
        let next = temp.path().join("next");
        import(&old, &next).unwrap();
        let config = Config::load(&next.join("sunshine.conf")).unwrap();
        for key in [
            "file_state",
            "file_apps",
            "credentials_file",
            "cert",
            "pkey",
        ] {
            assert!(!config.values.contains_key(key), "{key}");
        }
        assert_eq!(
            config.get("vibeshine_file_state", ""),
            "vibeshine_state.json"
        );
        let apps = state::load_json(&next.join("apps.json"), serde_json::json!({})).unwrap();
        assert_eq!(apps["apps"][0]["name"], "Kept");
        // Without its key the certificate is useless; the host makes a new pair.
        assert!(!next.join("credentials/cacert.pem").exists());
        assert!(old.join("credentials/cacert.pem").is_file());
    }
    #[test]
    fn logs_and_oversized_files_and_covers_are_left_out() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old");
        std::fs::create_dir_all(old.join("logs")).unwrap();
        let sized = |path: &Path, bytes: &[u8], size: u64| {
            let mut file = std::fs::File::create(path).unwrap();
            std::io::Write::write_all(&mut file, bytes).unwrap();
            file.set_len(size).unwrap();
        };
        sized(&old.join("logs/sunshine.log"), b"log", 70 * 1024 * 1024);
        sized(&old.join("dump.bin"), b"dump", 65 * 1024 * 1024);
        let cover = temp.path().join("huge.png");
        sized(&cover, b"\x89PNG\r\n\x1a\n", 17 * 1024 * 1024);
        let apps = serde_json::json!({"apps":[{"name":"Game","image-path":cover}]});
        std::fs::write(old.join("apps.json"), apps.to_string()).unwrap();
        std::fs::write(old.join("sunshine.conf"), "sunshine_name = PC\n").unwrap();
        let next = temp.path().join("next");
        import(&old, &next).unwrap();
        assert!(!next.join("logs").exists());
        assert!(!next.join("dump.bin").exists());
        let imported = state::load_json(&next.join("apps.json"), serde_json::json!({})).unwrap();
        assert_eq!(imported, apps);
        assert_eq!(
            Config::load(&next.join("sunshine.conf"))
                .unwrap()
                .get("sunshine_name", ""),
            "PC"
        );
    }
    #[test]
    fn a_profile_the_host_would_refuse_fails_the_import() {
        let temp = tempfile::tempdir().unwrap();
        let cases: [&[(&str, &str)]; 4] = [
            &[("vibeshine_state.json", r#"{"root":[]}"#)],
            &[(
                "vibeshine_state.json",
                r#"{"root":{"app_id_aliases":{"a":{"aliases":7}}}}"#,
            )],
            &[("sunshine_credentials.json", r#"{"username":"admin"}"#)],
            &[
                ("credentials/cacert.pem", "not a certificate"),
                ("credentials/cakey.pem", "not a key"),
            ],
        ];
        for (number, files) in cases.into_iter().enumerate() {
            let old = temp.path().join(format!("old{number}"));
            std::fs::create_dir_all(old.join("credentials")).unwrap();
            std::fs::write(old.join("sunshine.conf"), "sunshine_name = PC\n").unwrap();
            for (file, text) in files {
                std::fs::write(old.join(file), text).unwrap();
            }
            let next = temp.path().join(format!("next{number}"));
            let error = format!("{:#}", import(&old, &next).unwrap_err());
            assert!(error.contains("from starting"), "{error}");
            assert!(!next.exists());
        }
        assert!(!std::fs::read_dir(temp.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".butterpollo-import-")
        }));
    }
    #[test]
    fn a_vibeshine_profile_imports_and_loads() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("config");
        std::fs::create_dir_all(old.join("logs")).unwrap();
        let library = temp.path().join("library");
        std::fs::create_dir(&library).unwrap();
        let cover = library.join("game.png");
        std::fs::write(&cover, b"\x89PNG\r\n\x1a\ncover").unwrap();
        let bom = |text: &str| [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat();
        let uuid = "9b7c0b6e-1c1a-4f0e-9a43-1f5d2f9e8a10";
        let apps = json!({"env":{},"apps":[{
            "name":"Game","uuid":uuid,"cmd":"game.exe","image-path":cover,
            "detached":["steam://rungameid/1"],
            "prep-cmd":[
                {"do":"before.cmd","undo":"after.cmd","elevated":"true"},
                {"do":"","undo":"","elevated":"false"}
            ]
        }]});
        std::fs::write(library.join("apps.json"), bom(&apps.to_string())).unwrap();
        let host = "5a2f6a4e-3b1d-4c55-9a77-0d0c6a3c2b11";
        let state = json!({"root":{"uniqueid":host,"named_devices":""}});
        std::fs::write(old.join("sunshine_state.json"), bom(&state.to_string())).unwrap();
        let conf = format!(
            "# Vibeshine\r\nsunshine_name = \"Living room # TV\"\r\nfile_apps = {}\r\ncredentials_file = {}\r\n",
            library.join("apps.json").display(),
            temp.path().join("gone").join("credentials.json").display()
        );
        std::fs::write(old.join("sunshine.conf"), bom(&conf)).unwrap();
        std::fs::File::create(old.join("logs/sunshine.log"))
            .unwrap()
            .set_len(70 * 1024 * 1024)
            .unwrap();
        let next = temp.path().join("butterpollo");
        import(&old, &next).unwrap();
        check(&next).unwrap();
        assert!(!next.join("logs").exists());
        let config = Config::load(&next.join("sunshine.conf")).unwrap();
        assert_eq!(config.get("sunshine_name", ""), "\"Living room # TV\"");
        assert_eq!(config.get("file_apps", ""), "apps.json");
        assert!(!config.values.contains_key("credentials_file"));
        let paired = state::PairedState::load(&next.join("sunshine_state.json")).unwrap();
        assert_eq!(paired.unique_id, host);
        assert!(paired.clients.is_empty());
        let imported = state::load_json(&next.join("apps.json"), json!({})).unwrap();
        let apps: Vec<state::App> = serde_json::from_value(imported["apps"].clone()).unwrap();
        assert!(apps[0].prep[0].elevated && !apps[0].prep[1].elevated);
        assert_eq!(apps[0].extra["uuid"], uuid);
        assert_eq!(apps[0].extra["detached"], json!(["steam://rungameid/1"]));
        let image = PathBuf::from(apps[0].extra["image-path"].as_str().unwrap());
        assert!(image.starts_with(next.canonicalize().unwrap()));
    }
    #[test]
    fn covers_survive_original_profile_removal_and_a_failed_import_rolls_back() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old");
        std::fs::create_dir(&old).unwrap();
        let cover = old.join("game.png");
        let bytes = b"\x89PNG\r\n\x1a\nexisting-artwork-fixture";
        std::fs::write(&cover, bytes).unwrap();
        let apps = serde_json::json!({"apps":[{"name":"game","uuid":"same-identity","cmd":"original command","image-path":cover,"unknown":{"keep":true}}]});
        std::fs::write(old.join("apps.json"), serde_json::to_vec(&apps).unwrap()).unwrap();
        std::fs::write(old.join("sunshine.conf"), "future=keep\n").unwrap();
        let destination = temp.path().join("rust");
        import(&old, &destination).unwrap();
        let imported =
            state::load_json(&destination.join("apps.json"), serde_json::json!({})).unwrap();
        let owned = PathBuf::from(imported["apps"][0]["image-path"].as_str().unwrap());
        assert!(owned.starts_with(destination.canonicalize().unwrap()));
        std::fs::remove_dir_all(&old).unwrap();
        assert_eq!(std::fs::read(owned).unwrap(), bytes);
        assert_eq!(imported["apps"][0]["uuid"], "same-identity");
        assert_eq!(imported["apps"][0]["cmd"], "original command");
        assert_eq!(imported["apps"][0]["unknown"], apps["apps"][0]["unknown"]);
        std::fs::create_dir(&old).unwrap();
        // The credentials cannot be left out, and they cannot be copied.
        std::fs::File::create(old.join("credentials.json"))
            .unwrap()
            .set_len(65 * 1024 * 1024)
            .unwrap();
        std::fs::write(
            old.join("sunshine.conf"),
            "credentials_file=credentials.json\n",
        )
        .unwrap();
        let failed = temp.path().join("failed");
        assert!(import(&old, &failed).is_err());
        assert!(!failed.exists());
        assert!(!std::fs::read_dir(temp.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".butterpollo-import-")
        }));
    }
}
