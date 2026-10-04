//! The release package appended to setup.exe: the executable, then the ZIP,
//! then a 16-byte footer (`BPSETUP1` and the ZIP's length, little endian).
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};

const MAGIC: &[u8; 8] = b"BPSETUP1";

/// A window onto part of a file, so the ZIP reader sees only the payload.
struct Slice {
    file: File,
    start: u64,
    length: u64,
    position: u64,
}
impl Read for Slice {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self.length.saturating_sub(self.position);
        let wanted = (buffer.len() as u64).min(remaining) as usize;
        if wanted == 0 {
            return Ok(0);
        }
        self.file
            .seek(SeekFrom::Start(self.start + self.position))?;
        let read = self.file.read(&mut buffer[..wanted])?;
        self.position += read as u64;
        Ok(read)
    }
}
impl Seek for Slice {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let next = match to {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::End(n) => self.length as i64 + n,
            SeekFrom::Current(n) => self.position as i64 + n,
        };
        if next < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek before the payload",
            ));
        }
        self.position = next as u64;
        Ok(self.position)
    }
}

/// The length of setup.exe without its payload, and the payload's range.
fn layout(path: &Path) -> Result<Option<(u64, u64)>> {
    let mut file = File::open(path)?;
    let total = file.metadata()?.len();
    if total < 16 {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-16))?;
    let mut footer = [0u8; 16];
    file.read_exact(&mut footer)?;
    if &footer[..8] != MAGIC {
        return Ok(None);
    }
    let length = u64::from_le_bytes(footer[8..].try_into().unwrap());
    if length == 0 || length > total - 16 {
        bail!("setup payload is damaged");
    }
    Ok(Some((total - 16 - length, length)))
}
pub struct Payload {
    archive: zip::ZipArchive<Slice>,
}
impl Payload {
    /// The payload of the running setup.exe; None for a build without one.
    pub fn open() -> Result<Option<Self>> {
        let exe = std::env::current_exe()?;
        let Some((start, length)) = layout(&exe)? else {
            return Ok(None);
        };
        let slice = Slice {
            file: File::open(&exe)?,
            start,
            length,
            position: 0,
        };
        Ok(Some(Self {
            archive: zip::ZipArchive::new(slice).context("reading the setup payload")?,
        }))
    }
    /// Extract into `destination`, dropping the archive's top folder.
    /// Entries escaping the destination are rejected.
    pub fn extract(&mut self, destination: &Path) -> Result<()> {
        for index in 0..self.archive.len() {
            let mut entry = self.archive.by_index(index)?;
            let Some(name) = entry.enclosed_name() else {
                bail!("unsafe path in the setup payload");
            };
            let relative: PathBuf = name.components().skip(1).collect();
            if relative.as_os_str().is_empty() {
                continue;
            }
            if relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            {
                bail!("unsafe path in the setup payload");
            }
            let target = destination.join(&relative);
            if entry.is_dir() {
                std::fs::create_dir_all(&target)?;
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = File::create(&target)?;
            std::io::copy(&mut entry, &mut file)?;
        }
        Ok(())
    }
}
/// Copy setup.exe without its payload, to serve as the uninstaller.
pub fn write_stub(destination: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let length = layout(&exe)?.map_or(std::fs::metadata(&exe)?.len(), |(start, _)| start);
    let mut source = File::open(&exe)?.take(length);
    let mut target = File::create(destination)?;
    std::io::copy(&mut source, &mut target)?;
    Ok(())
}

#[derive(serde::Deserialize)]
pub struct Entry {
    pub path: String,
    pub sha256: String,
}
/// The package's file list, checked against the files in `root`.
pub fn verify(root: &Path) -> Result<Vec<Entry>> {
    let entries = manifest(root)?;
    for entry in &entries {
        let path = safe_join(root, &entry.path)?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut File::open(&path)?, &mut hasher)
            .with_context(|| format!("reading {}", entry.path))?;
        if hex(&hasher.finalize()) != entry.sha256.to_ascii_lowercase() {
            bail!("{} does not match the package manifest", entry.path);
        }
    }
    Ok(entries)
}
pub fn manifest(root: &Path) -> Result<Vec<Entry>> {
    let text = std::fs::read_to_string(root.join("manifest.json"))?;
    let text = text.trim_start_matches('\u{feff}');
    // PowerShell writes a single entry as an object rather than an array.
    let value: serde_json::Value = serde_json::from_str(text)?;
    Ok(match value {
        serde_json::Value::Array(_) => serde_json::from_value(value)?,
        other => vec![serde_json::from_value(other)?],
    })
}
pub fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("unsafe path in the package manifest");
    }
    Ok(root.join(relative))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_paths_cannot_escape_the_package() {
        let root = Path::new("C:\\package");
        assert!(safe_join(root, "drivers\\gamepad\\install.ps1").is_ok());
        assert!(safe_join(root, "..\\Windows\\evil.dll").is_err());
        assert!(safe_join(root, "C:\\Windows\\evil.dll").is_err());
    }
    #[test]
    fn payload_footer_locates_the_archive() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("setup.exe");
        let mut bytes = b"stub".to_vec();
        bytes.extend_from_slice(b"zipdata");
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&7u64.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(layout(&path).unwrap(), Some((4, 7)));
        std::fs::write(&path, b"plain executable without payload").unwrap();
        assert_eq!(layout(&path).unwrap(), None);
    }
}
