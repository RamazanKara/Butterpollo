//! Local minidumps for unhandled native exceptions and Rust panics.
use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use windows::Win32::{
    Foundation::HANDLE,
    System::{Diagnostics::Debug::*, Threading::*},
};
static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
static WRITING: AtomicBool = AtomicBool::new(false);
fn dump(exceptions: Option<*mut EXCEPTION_POINTERS>) -> Result<()> {
    if WRITING.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let result = (|| -> Result<()> {
        use std::os::windows::io::AsRawHandle;
        let directory = DIRECTORY.get().context("crash reporting not initialized")?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        let file = std::fs::File::create(
            directory.join(format!("butterpollo.{}.{stamp}.dmp", std::process::id())),
        )?;
        unsafe {
            let info = exceptions.map(|p| MINIDUMP_EXCEPTION_INFORMATION {
                ThreadId: GetCurrentThreadId(),
                ExceptionPointers: p,
                ClientPointers: false.into(),
            });
            MiniDumpWriteDump(
                GetCurrentProcess(),
                GetCurrentProcessId(),
                HANDLE(file.as_raw_handle()),
                MiniDumpNormal | MiniDumpWithThreadInfo | MiniDumpWithUnloadedModules,
                info.as_ref().map(|i| i as *const _),
                None,
                None,
            )?;
        }
        file.sync_all()?;
        Ok(())
    })();
    WRITING.store(false, Ordering::Release);
    result
}
unsafe extern "system" fn unhandled(exceptions: *const EXCEPTION_POINTERS) -> i32 {
    let _ = dump(Some(exceptions.cast_mut()));
    1 // EXCEPTION_EXECUTE_HANDLER
}
pub fn initialize(directory: &Path) -> Result<()> {
    let directory = directory.join("crashes");
    std::fs::create_dir_all(&directory)?;
    DIRECTORY
        .set(directory)
        .map_err(|_| anyhow::anyhow!("crash reporting already initialized"))?;
    unsafe {
        SetUnhandledExceptionFilter(Some(unhandled));
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(directory) = DIRECTORY.get() {
            let _ = std::fs::write(directory.join("panic.txt"), info.to_string());
        }
        let _ = dump(None);
        previous(info);
    }));
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_report_has_the_windows_minidump_signature() {
        let directory = tempfile::tempdir().unwrap();
        super::initialize(directory.path()).unwrap();
        super::dump(None).unwrap();
        let report = std::fs::read_dir(directory.path().join("crashes"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let bytes = std::fs::read(report).unwrap();
        assert_eq!(&bytes[..4], b"MDMP");
        assert!(bytes.len() > 32);
    }
}
