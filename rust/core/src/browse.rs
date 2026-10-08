//! Folder listings for the console's file picker (`/api/browse`), as in
//! Vibepollo: folders first, then the files the picker asks for.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Which files a listing includes; folders are always listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Any,
    File,
    Executable,
}
impl Filter {
    /// The `type` query value: `file`, `executable`, else everything.
    pub fn parse(value: &str) -> Self {
        match value {
            "file" => Self::File,
            "executable" => Self::Executable,
            _ => Self::Any,
        }
    }
}
/// Programs the app editor can run.
fn executable(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["exe", "bat", "cmd", "ps1"].contains(&e.to_ascii_lowercase().as_str()))
}
/// Whether `path` asks for the list of drives rather than a folder.
pub fn root(path: &str) -> bool {
    matches!(path.trim(), "" | "\\" | "/")
}
/// The listing of drives, e.g. `C:\`.
pub fn drives(drives: &[String]) -> Value {
    let entries: Vec<Value> = drives
        .iter()
        .map(|d| json!({"name": d, "path": d, "type": "directory"}))
        .collect();
    json!({"path": "", "parent": "", "entries": entries})
}
/// The folder at `path`, or the nearest existing folder above it. Its
/// `parent` is empty at a drive's root, which goes back to the drives.
pub fn listing(path: &Path, filter: Filter) -> Result<Value, String> {
    let mut folder = PathBuf::from(path);
    if folder.is_file() {
        folder.pop();
    }
    while !folder.as_os_str().is_empty() && !folder.exists() {
        if !folder.pop() {
            break;
        }
    }
    if folder.as_os_str().is_empty() || !folder.is_dir() {
        return Err("Directory does not exist".into());
    }
    let mut entries: Vec<(bool, String, Value)> = std::fs::read_dir(&folder)
        .map_err(|e| format!("cannot read {}: {e}", folder.display()))?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            // Follows links, as a program started from the picker would.
            let metadata = std::fs::metadata(&path).ok()?;
            let directory = metadata.is_dir();
            let included = directory
                || match filter {
                    Filter::Any => true,
                    Filter::File => metadata.is_file(),
                    Filter::Executable => metadata.is_file() && executable(&path),
                };
            let name = entry.file_name().to_string_lossy().into_owned();
            included.then(|| {
                let value = json!({
                    "name": name,
                    "path": path.to_string_lossy(),
                    "type": if directory { "directory" } else { "file" },
                });
                (directory, name.to_lowercase(), value)
            })
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let parent = folder
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(json!({
        "path": folder.to_string_lossy(),
        "parent": parent,
        "entries": entries.into_iter().map(|(_, _, value)| value).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(listing: &Value) -> Vec<(String, String)> {
        listing["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["name"].as_str().unwrap().to_owned(),
                    e["type"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
    #[test]
    fn folders_come_first_and_the_filter_picks_the_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("Games")).unwrap();
        std::fs::create_dir(d.path().join("apps")).unwrap();
        for file in ["readme.txt", "Game.EXE", "start.bat", "setup.msi"] {
            std::fs::write(d.path().join(file), b"x").unwrap();
        }
        let pair = |name: &str, kind: &str| (name.to_owned(), kind.to_owned());
        let all = listing(d.path(), Filter::Any).unwrap();
        assert_eq!(
            names(&all),
            [
                pair("apps", "directory"),
                pair("Games", "directory"),
                pair("Game.EXE", "file"),
                pair("readme.txt", "file"),
                pair("setup.msi", "file"),
                pair("start.bat", "file"),
            ]
        );
        assert_eq!(all["path"], d.path().to_string_lossy().as_ref());
        assert_eq!(
            all["parent"],
            d.path().parent().unwrap().to_string_lossy().as_ref()
        );
        let programs = listing(d.path(), Filter::Executable).unwrap();
        assert_eq!(
            names(&programs),
            [
                pair("apps", "directory"),
                pair("Games", "directory"),
                pair("Game.EXE", "file"),
                pair("start.bat", "file"),
            ]
        );
        assert_eq!(Filter::parse("executable"), Filter::Executable);
        assert_eq!(Filter::parse("file"), Filter::File);
        assert_eq!(Filter::parse("folders"), Filter::Any);
    }
    #[test]
    fn a_file_or_a_missing_path_lists_the_nearest_folder() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("game.exe"), b"x").unwrap();
        let expected = d.path().to_string_lossy().into_owned();
        for path in [
            d.path().join("game.exe"),
            d.path().join("missing").join("deeper"),
        ] {
            let listed = listing(&path, Filter::Any).unwrap();
            assert_eq!(listed["path"], expected.as_str());
        }
        assert!(listing(Path::new("missing-relative-folder"), Filter::Any).is_err());
    }
    #[test]
    fn an_empty_path_or_a_slash_lists_the_drives() {
        for path in ["", " ", "\\", "/"] {
            assert!(root(path));
        }
        assert!(!root("C:\\"));
        let listed = drives(&["C:\\".into(), "D:\\".into()]);
        assert_eq!(listed["path"], "");
        assert_eq!(listed["entries"][1]["path"], "D:\\");
        assert_eq!(listed["entries"][0]["type"], "directory");
    }
}
