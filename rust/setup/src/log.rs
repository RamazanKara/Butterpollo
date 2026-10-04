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
    let path = std::env::temp_dir().join(format!("butterpollo-setup-{seconds}.log"));
    let file = std::fs::File::create(&path).ok()?;
    *LOG.lock().unwrap() = Some(Log {
        path: path.clone(),
        file,
        started: Instant::now(),
    });
    line(format!(
        "Butterpollo setup {} (unix time {seconds})",
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
