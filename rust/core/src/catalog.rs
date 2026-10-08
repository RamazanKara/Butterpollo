//! Existing CRC32 application identities, artwork versions and cached aliases.
use crate::{crypto, state::App};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Default, Serialize, Deserialize)]
struct AliasState {
    #[serde(default)]
    current_id: String,
    #[serde(default)]
    cover_fingerprint: String,
    #[serde(default, deserialize_with = "aliases")]
    aliases: BTreeSet<String>,
}
fn aliases<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<BTreeSet<String>, D::Error> {
    match Value::deserialize(d)? {
        Value::String(s) if s.is_empty() => Ok(BTreeSet::new()),
        Value::Array(a) => a
            .into_iter()
            .map(|v| {
                v.as_str()
                    .map(|s| s.trim().to_owned())
                    .ok_or_else(|| serde::de::Error::custom("invalid app alias"))
            })
            .collect(),
        _ => Err(serde::de::Error::custom("invalid app aliases")),
    }
}
fn crc(s: &str) -> String {
    (crc32fast::hash(s.as_bytes()) as i32)
        .unsigned_abs()
        .to_string()
}
/// A PNG file of at most 16 MiB.
fn png(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut header = [0; 8];
    file.metadata()
        .is_ok_and(|m| m.is_file() && m.len() <= 16 * 1024 * 1024)
        && file.read_exact(&mut header).is_ok()
        && &header == b"\x89PNG\r\n\x1a\n"
}
/// The icon a Playnite sync saved for `app` (`playnite-icon-path`), served
/// as `/api/apps/{uuid}/icon`. Only a PNG, like the covers.
pub fn icon(app: &App) -> Option<PathBuf> {
    let path = app
        .extra
        .get("playnite-icon-path")
        .and_then(Value::as_str)?
        .trim();
    (!path.is_empty() && png(Path::new(path))).then(|| PathBuf::from(path))
}
pub fn artwork(app: &App, assets: &Path) -> PathBuf {
    let image = app
        .extra
        .get("image-path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let fallback = assets.join("box.png");
    if !image.to_ascii_lowercase().ends_with(".png") {
        return fallback;
    }
    let valid = png;
    let candidate = assets.join(image);
    if valid(&candidate) {
        candidate
    } else if image == "./assets/steam.png" {
        assets.join("steam.png")
    } else if valid(Path::new(image)) {
        PathBuf::from(image)
    } else {
        fallback
    }
}
pub fn assign(apps: &mut [App], document: &mut Value, assets: &Path) -> Result<()> {
    if !document.is_object() {
        bail!("app alias state must be an object");
    }
    if document.get("root").is_none() {
        document["root"] = json!({});
    }
    if !document["root"].is_object() {
        bail!("app alias root must be an object");
    }
    let v = document["root"]
        .get("app_id_aliases")
        .cloned()
        .unwrap_or(json!({}));
    let mut persisted: BTreeMap<String, AliasState> = if v == json!("") {
        BTreeMap::new()
    } else {
        serde_json::from_value(v).context("invalid app ID aliases")?
    };
    let mut occupied = BTreeSet::new();
    let mut active = BTreeSet::new();
    for (index, app) in apps.iter_mut().enumerate() {
        app.aliases.clear();
        let uuid = app.extra.get("uuid").and_then(Value::as_str).unwrap_or("");
        let path = artwork(app, assets);
        let hash = if path == assets.join("box.png") {
            None
        } else {
            std::fs::read(&path)
                .ok()
                .map(|b| hex::encode(crypto::hash(&b)))
        };
        let legacy = if uuid.is_empty() {
            format!("{}{}", app.name, hash.clone().unwrap_or_default())
        } else {
            uuid.to_owned()
        };
        let plain = crc(&legacy);
        let indexed = crc(&format!("{legacy}{index}"));
        if uuid.is_empty() {
            app.computed_id = Some(
                if occupied.contains(&plain) {
                    indexed
                } else {
                    plain
                }
                .parse()?,
            );
        } else {
            active.insert(uuid.to_owned());
            let fingerprint = hash
                .map(|h| format!("sha256:{h}"))
                .unwrap_or_else(|| "default".into());
            let state = persisted
                .entry(uuid.to_owned())
                .or_insert_with(|| AliasState {
                    current_id: plain.clone(),
                    cover_fingerprint: fingerprint.clone(),
                    aliases: BTreeSet::new(),
                });
            if state.current_id.is_empty() {
                state.current_id = plain.clone();
            }
            if state.cover_fingerprint.is_empty() {
                state.cover_fingerprint = fingerprint.clone();
            }
            let versioned = crc(&format!("{uuid}\n{fingerprint}"));
            let versioned_index = crc(&format!("{uuid}\n{fingerprint}{index}"));
            if state.cover_fingerprint != fingerprint || state.current_id == plain {
                let previous = state.current_id.clone();
                state.current_id = if occupied.contains(&versioned) {
                    versioned_index.clone()
                } else {
                    versioned
                };
                state.cover_fingerprint = fingerprint;
                state.aliases.insert(previous);
                state.aliases.insert(plain.clone());
            }
            if occupied.contains(&state.current_id) {
                state.aliases.insert(state.current_id.clone());
                state.current_id = if occupied.contains(&versioned_index) {
                    indexed
                } else {
                    versioned_index
                };
            }
            app.computed_id = Some(state.current_id.parse()?);
            app.aliases = state
                .aliases
                .iter()
                .map(|s| s.parse())
                .collect::<std::result::Result<Vec<_>, _>>()?;
        }
        if !occupied.insert(app.id().to_string()) {
            bail!("duplicate application identity");
        }
    }
    persisted.retain(|uuid, _| active.contains(uuid));
    let mut counts = BTreeMap::new();
    for app in apps.iter() {
        for alias in &app.aliases {
            *counts.entry(*alias).or_insert(0usize) += 1;
        }
    }
    for app in apps {
        app.aliases
            .retain(|id| !occupied.contains(&id.to_string()) && counts[id] == 1);
        if let Some(uuid) = app.extra.get("uuid").and_then(Value::as_str)
            && let Some(state) = persisted.get_mut(uuid)
        {
            state.aliases = app.aliases.iter().map(u32::to_string).collect();
        }
    }
    document["root"]["app_id_aliases"] = serde_json::to_value(persisted)?;
    Ok(())
}

/// Previous reorder semantics: ignore unknown/duplicate IDs, then append every
/// application omitted by the caller in its existing relative order.
pub fn reorder(apps: &[App], order: &[Value]) -> Vec<App> {
    let mut moved = std::collections::BTreeSet::new();
    let mut next = Vec::with_capacity(apps.len());
    for id in order.iter().filter_map(Value::as_str) {
        if let Some((index, app)) = apps.iter().enumerate().find(|(index, app)| {
            !moved.contains(index) && app.extra.get("uuid").and_then(Value::as_str) == Some(id)
        }) {
            moved.insert(index);
            next.push(app.clone());
        }
    }
    next.extend(
        apps.iter()
            .enumerate()
            .filter(|(index, _)| !moved.contains(index))
            .map(|(_, app)| app.clone()),
    );
    next
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reorder_ignores_invalid_ids_and_preserves_omitted_apps_and_unknown_fields() {
        let apps: Vec<App> = ["a", "b", "c", "d"]
            .iter()
            .map(|uuid| {
                serde_json::from_value(json!({"name":uuid,"uuid":uuid,"custom":{"nested":true}}))
                    .unwrap()
            })
            .collect();
        let ordered = reorder(
            &apps,
            &[
                json!("c"),
                json!(17),
                json!(null),
                json!("c"),
                json!("missing"),
                json!("a"),
            ],
        );
        assert_eq!(
            ordered
                .iter()
                .map(|a| a.extra["uuid"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["c", "a", "b", "d"]
        );
        assert!(ordered.iter().all(|a| a.extra["custom"]["nested"] == true));
    }
    #[test]
    fn renaming_preserves_uuid_and_old_ids_resolve_after_cover_changes() {
        let d = tempfile::tempdir().unwrap();
        let mut a = App::desktop();
        a.extra
            .insert("uuid".into(), json!("f773d31b-43da-470c-80d5-02e777a6d993"));
        let mut apps = vec![a];
        let mut doc = json!({"root":{"other":123}});
        assign(&mut apps, &mut doc, d.path()).unwrap();
        let old = apps[0].id();
        assert!(
            apps[0]
                .aliases
                .contains(&crc("f773d31b-43da-470c-80d5-02e777a6d993").parse().unwrap())
        );
        apps[0].name = "Renamed".into();
        assign(&mut apps, &mut doc, d.path()).unwrap();
        assert_eq!(old, apps[0].id());
        std::fs::write(d.path().join("cover.png"), b"\x89PNG\r\n\x1a\nnew-cover").unwrap();
        apps[0]
            .extra
            .insert("image-path".into(), json!("cover.png"));
        assign(&mut apps, &mut doc, d.path()).unwrap();
        assert_ne!(old, apps[0].id());
        assert!(apps[0].aliases.contains(&old));
        assert_eq!(doc["root"]["other"], 123);
    }
    #[test]
    fn the_icon_is_the_synced_png_and_nothing_else() {
        let d = tempfile::tempdir().unwrap();
        let mut app = App::desktop();
        assert_eq!(icon(&app), None);
        let png = d.path().join("playnite_icon_game.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nicon").unwrap();
        let text = d.path().join("secrets.txt");
        std::fs::write(&text, b"not an image").unwrap();
        for (path, expected) in [
            (png.clone(), Some(png.clone())),
            (text, None),
            (d.path().join("missing.png"), None),
            (d.path().to_path_buf(), None),
        ] {
            app.extra.insert(
                "playnite-icon-path".into(),
                json!(format!(" {} ", path.display())),
            );
            assert_eq!(icon(&app), expected);
        }
    }
}
