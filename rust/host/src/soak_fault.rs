//! Faults for the isolated debug host used by the release soak. Never compiled
//! into a release binary or enabled without an explicit test-owned directory.
use anyhow::{Context, Result};
use butterpollo_windows::encoder::Encoded;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant},
};

fn directory() -> Option<&'static Path> {
    static DIRECTORY: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIRECTORY
        .get_or_init(|| std::env::var_os("BUTTERPOLLO_TEST_FAULT_DIR").map(PathBuf::from))
        .as_deref()
}

pub fn check(kind: &str) -> Result<()> {
    if let Some(directory) = directory() {
        check_at(directory, kind)?;
    }
    Ok(())
}

/// Withhold completed output at the session's encoder boundary. Held frames
/// count as backlog; recreating the encoder discards them but not the fault.
pub struct EncoderStall {
    directory: Option<PathBuf>,
    until: Option<Instant>,
    pub held: Vec<Encoded>,
}
impl Default for EncoderStall {
    fn default() -> Self {
        Self::new(directory().map(Path::to_path_buf))
    }
}
impl EncoderStall {
    pub fn new(directory: Option<PathBuf>) -> Self {
        Self {
            directory,
            until: None,
            held: vec![],
        }
    }
    pub fn output(&mut self, output: Vec<Encoded>, now: Instant) -> Result<Vec<Encoded>> {
        let Some(directory) = &self.directory else {
            return Ok(output);
        };
        let request = directory.join("encoder stall");
        match std::fs::read_to_string(&request) {
            Ok(duration) => {
                let duration = Duration::from_millis(
                    duration
                        .trim()
                        .parse()
                        .context("encoder stall must contain milliseconds")?,
                );
                self.until = Some(
                    now.checked_add(duration)
                        .context("encoder stall duration too large")?,
                );
                std::fs::remove_file(request)?;
                tracing::info!(
                    duration_ms = duration.as_millis(),
                    "soak injected encoder stall"
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if directory.join("encoder stall.persistent").exists()
            || self.until.is_some_and(|until| now < until)
        {
            self.held.extend(output);
            return Ok(vec![]);
        }
        self.until = None;
        if self.held.is_empty() {
            return Ok(output);
        }
        let mut resumed = std::mem::take(&mut self.held);
        resumed.extend(output);
        Ok(resumed)
    }
}

pub(crate) fn check_at(directory: &Path, kind: &str) -> Result<()> {
    if directory.join(format!("{kind}.persistent")).exists() {
        return Err(fault(kind, true));
    }
    let request = directory.join(kind);
    match std::fs::remove_file(request) {
        Ok(()) => Err(fault(kind, false)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
fn fault(kind: &str, persistent: bool) -> anyhow::Error {
    let error = if persistent {
        anyhow::anyhow!("soak injected persistent {kind}")
    } else {
        anyhow::anyhow!("soak injected {kind}")
    };
    if kind == "DXGI_ERROR_DEVICE_REMOVED" {
        error.context(butterpollo_windows::device_loss::DeviceLost(
            0x887a0005_u32 as i32,
        ))
    } else {
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fault_is_consumed_once_and_cannot_affect_another_stage() -> Result<()> {
        let directory =
            std::env::temp_dir().join(format!("butterpollo-soak-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        std::fs::write(directory.join("DXGI_ERROR_ACCESS_LOST"), [])?;
        check_at(&directory, "encoder failure")?;
        assert!(check_at(&directory, "DXGI_ERROR_ACCESS_LOST").is_err());
        check_at(&directory, "DXGI_ERROR_ACCESS_LOST")?;
        let persistent = directory.join("encoder failure.persistent");
        std::fs::write(&persistent, [])?;
        assert!(check_at(&directory, "encoder failure").is_err());
        assert!(check_at(&directory, "encoder failure").is_err());
        std::fs::remove_file(persistent)?;
        check_at(&directory, "encoder failure")?;
        std::fs::remove_dir(directory)?;
        Ok(())
    }
}
