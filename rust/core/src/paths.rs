//! Where the host keeps its profile on Windows.
use anyhow::{Context, Result};
use std::path::PathBuf;

/// The installed service's profile, `%ProgramData%\Butterpollo\config`.
/// Falls back to `C:\ProgramData` when the variable is unset, as it is for
/// some service accounts.
pub fn installed_profile() -> PathBuf {
    PathBuf::from(std::env::var_os("PROGRAMDATA").unwrap_or_else(|| r"C:\ProgramData".into()))
        .join("Butterpollo/config")
}

/// A portable host's profile, `%LocalAppData%\ButterpolloRust\config`.
pub fn portable_profile() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?)
            .join("ButterpolloRust/config"),
    )
}
