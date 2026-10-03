//! Size-limited log files. When `name` would exceed its limit it becomes
//! `name.1`, older files move up to `name.<keep>` and the oldest is dropped,
//! so a long-running service cannot fill the disk.
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

pub struct RotatingFile {
    path: PathBuf,
    file: File,
    size: u64,
    limit: u64,
    keep: usize,
}
impl RotatingFile {
    /// The host's default: 32 MiB per file and four older files.
    pub fn open_default(path: &Path) -> io::Result<Self> {
        Self::open(path, 32 * 1024 * 1024, 4)
    }
    pub fn open(path: &Path, limit: u64, keep: usize) -> io::Result<Self> {
        let mut log = Self {
            path: path.to_owned(),
            file: append(path)?,
            size: std::fs::metadata(path).map_or(0, |m| m.len()),
            limit: limit.max(1),
            keep: keep.max(1),
        };
        if log.size >= log.limit {
            log.rotate()?;
        }
        Ok(log)
    }
    /// `name.<n>`; also the rotated files a log viewer or bundle may include.
    pub fn rotated(path: &Path, n: usize) -> PathBuf {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }
    fn rotate(&mut self) -> io::Result<()> {
        let _ = std::fs::remove_file(Self::rotated(&self.path, self.keep));
        for n in (1..self.keep).rev() {
            match std::fs::rename(
                Self::rotated(&self.path, n),
                Self::rotated(&self.path, n + 1),
            ) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        // Windows allows renaming the open file: Rust opens files shareable
        // for deletion, as do readers that follow the log.
        std::fs::rename(&self.path, Self::rotated(&self.path, 1))?;
        self.file = append(&self.path)?;
        self.size = 0;
        Ok(())
    }
}
fn append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}
impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // If rotation fails (a reader without delete sharing holds the old
        // file), keep writing to the current file rather than lose lines.
        if self.size > 0 && self.size + buf.len() as u64 > self.limit {
            let _ = self.rotate();
        }
        let written = self.file.write(buf)?;
        self.size += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logs_roll_over_and_keep_a_bounded_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("host.log");
        let mut log = RotatingFile::open(&path, 100, 2).unwrap();
        for line in 0..10 {
            log.write_all(format!("{line:039}\n").as_bytes()).unwrap();
        }
        drop(log);
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
        assert!(size(&path).unwrap() <= 100);
        assert!(size(&RotatingFile::rotated(&path, 1)).unwrap() <= 100);
        assert!(size(&RotatingFile::rotated(&path, 2)).is_some());
        assert!(size(&RotatingFile::rotated(&path, 3)).is_none());
        // The newest line is in the current file.
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .ends_with(&format!("{:039}\n", 9))
        );
        // An oversized file rolls over when opened again.
        std::fs::write(&path, vec![b'x'; 150]).unwrap();
        RotatingFile::open(&path, 100, 2).unwrap();
        assert_eq!(size(&path), Some(0));
        assert_eq!(size(&RotatingFile::rotated(&path, 1)), Some(150));
    }
}
