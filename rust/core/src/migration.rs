//! Copy an existing Windows profile into an empty, independent Rust profile.
use crate::{config::Config, state};
use anyhow::{Context, Result, bail};
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
fn copy_file(source: &Path, destination: &Path, files: &mut usize, total: &mut u64) -> Result<()> {
    let metadata = source.symlink_metadata()?;
    if linked(&metadata) {
        bail!("profile file is a link: {}", source.display());
    }
    *files += 1;
    *total = total.saturating_add(metadata.len());
    if *files > 10000 || metadata.len() > 64 * 1024 * 1024 || *total > 512 * 1024 * 1024 {
        bail!("profile exceeds migration size limits");
    }
    std::fs::create_dir_all(
        destination
            .parent()
            .context("profile file needs a directory")?,
    )?;
    std::fs::copy(source, destination)?;
    Ok(())
}
pub fn profile_id(directory: &Path) -> String {
    let canonical = directory
        .canonicalize()
        .unwrap_or_else(|_| directory.to_path_buf());
    hex::encode(Sha256::digest(
        canonical.to_string_lossy().to_lowercase().as_bytes(),
    ))
}
fn copy_tree(source: &Path, destination: &Path, files: &mut usize, total: &mut u64) -> Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let metadata = entry.path().symlink_metadata()?;
        if linked(&metadata) {
            bail!(
                "profile contains a symbolic link: {}",
                entry.path().display()
            );
        }
        if metadata.is_dir() {
            copy_tree(
                &entry.path(),
                &destination.join(entry.file_name()),
                files,
                total,
            )?;
        } else if metadata.is_file() {
            copy_file(
                &entry.path(),
                &destination.join(entry.file_name()),
                files,
                total,
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
        copy_tree(&source, &stage, &mut files, &mut total)?;
        let mut rewritten = config.clone();
        for (key, target) in [
            ("file_state", "sunshine_state.json"),
            ("file_apps", "apps.json"),
            ("vibeshine_file_state", "vibeshine_state.json"),
            ("credentials_file", "sunshine_credentials.json"),
            ("cert", "credentials/cacert.pem"),
            ("pkey", "credentials/cakey.pem"),
        ] {
            if !config.values.contains_key(key) {
                continue;
            }
            let original = config.path(key, &source, target);
            if original.is_file() {
                let path = stage.join(target);
                copy_file(&original, &path, &mut files, &mut total)?;
                rewritten.values.insert(key.into(), target.into());
            } else if matches!(
                key,
                "file_state" | "file_apps" | "credentials_file" | "cert" | "pkey"
            ) {
                bail!("configured profile file is missing: {}", original.display());
            } else {
                rewritten.values.insert(key.into(), target.into());
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
                    let metadata = original.symlink_metadata()?;
                    if linked(&metadata) || metadata.len() > 16 * 1024 * 1024 {
                        bail!("invalid profile cover: {}", original.display());
                    }
                    let bytes = std::fs::read(&original)?;
                    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                        continue;
                    }
                    let target = PathBuf::from("covers")
                        .join(format!("{}.png", hex::encode(Sha256::digest(&bytes))));
                    if !stage.join(&target).is_file() {
                        copy_file(&original, &stage.join(&target), &mut files, &mut total)?;
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
    fn covers_survive_original_profile_removal_and_failed_identity_import_rolls_back() {
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
        std::fs::write(
            old.join("sunshine.conf"),
            "cert=missing.pem\npkey=missing-key.pem\n",
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
