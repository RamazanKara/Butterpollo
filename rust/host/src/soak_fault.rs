//! Faults for the isolated debug host used by the release soak. Never compiled
//! into a release binary or enabled without an explicit test-owned directory.
use anyhow::Result;
use std::{path::Path, sync::OnceLock};

pub fn check(kind: &str) -> Result<()> {
    static DIRECTORY: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    if let Some(directory) = DIRECTORY.get_or_init(|| {
        std::env::var_os("BUTTERPOLLO_TEST_FAULT_DIR").map(std::path::PathBuf::from)
    }) {
        check_at(directory, kind)?;
    }
    Ok(())
}

fn check_at(directory: &Path, kind: &str) -> Result<()> {
    if directory.join(format!("{kind}.persistent")).exists() {
        anyhow::bail!("soak injected persistent {kind}");
    }
    let request = directory.join(kind);
    match std::fs::remove_file(request) {
        Ok(()) => anyhow::bail!("soak injected {kind}"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
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
