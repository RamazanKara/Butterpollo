//! The setup log in %TEMP%, kept for support whether setup succeeds or not.
use std::{
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

struct Log {
    path: PathBuf,
    file: std::fs::File,
    started: Instant,
}
static LOG: Mutex<Option<Log>> = Mutex::new(None);

/// Open `%TEMP%\butterpollo-setup-<unix time>.log`.
pub fn open() -> Option<PathBuf> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    open_at(
        std::env::temp_dir().join(format!("butterpollo-setup-{seconds}.log")),
        false,
    )
}
/// Open the log at `path`, after what it holds if `append`.
pub fn open_at(path: PathBuf, append: bool) -> Option<PathBuf> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&path)
        .ok()?;
    *LOG.lock().unwrap() = Some(Log {
        path: path.clone(),
        file,
        started: Instant::now(),
    });
    line(format!(
        "Rubylight setup {} (unix time {seconds})",
        env!("CARGO_PKG_VERSION")
    ));
    Some(path)
}
pub fn path() -> Option<PathBuf> {
    LOG.lock().unwrap().as_ref().map(|log| log.path.clone())
}
pub fn line(text: impl AsRef<str>) {
    if let Some(log) = LOG.lock().unwrap().as_mut() {
        let elapsed = log.started.elapsed().as_secs_f64();
        for line in text.as_ref().lines() {
            let _ = writeln!(log.file, "[{elapsed:8.3}] {line}");
        }
        let _ = log.file.flush();
    }
}
