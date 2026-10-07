//! Publish complete package files without truncating the installed version.
use anyhow::{Context, Result};
use std::{fs::File, path::Path};

pub(crate) fn replace(
    source: &Path,
    target: &Path,
    remove_later: impl FnOnce(&Path),
) -> Result<()> {
    write(
        target,
        |file| {
            std::io::copy(&mut File::open(source)?, file)?;
            Ok(())
        },
        remove_later,
    )
}

pub(crate) fn write(
    target: &Path,
    contents: impl FnOnce(&mut File) -> Result<()>,
    remove_later: impl FnOnce(&Path),
) -> Result<()> {
    let parent = target.parent().context("package file needs a parent")?;
    std::fs::create_dir_all(parent)?;
    let id = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let temporary = parent.join(format!(".butterpollo-new-{id}"));
    let result = (|| -> Result<()> {
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        contents(&mut file)?;
        file.sync_all()?;
        drop(file);
        if let Err(error) = publish(&temporary, target) {
            // Windows can rename a loaded image even when it cannot replace it.
            #[cfg(windows)]
            if matches!(error.raw_os_error(), Some(5 | 32)) && target.is_file() {
                let aside = parent.join(format!(".butterpollo-old-{id}"));
                std::fs::rename(target, &aside)?;
                if let Err(error) = publish(&temporary, target) {
                    publish(&aside, target).with_context(|| {
                        format!(
                            "restoring {}; its previous file is at {}",
                            target.display(),
                            aside.display()
                        )
                    })?;
                    return Err(error.into());
                }
                remove_later(&aside);
                return Ok(());
            }
            return Err(error.into());
        }
        Ok(())
    })();
    #[cfg(not(windows))]
    let _ = remove_later;
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.with_context(|| format!("replacing {}", target.display()))
}

fn publish(source: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        unsafe extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let target: Vec<_> = target.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 0x1 | 0x8) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    std::fs::rename(source, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_copy_never_truncates_the_installed_file() -> Result<()> {
        let root = tempfile::tempdir()?;
        let target = root.path().join("Çağrı Müller/host.exe");
        std::fs::create_dir_all(target.parent().unwrap())?;
        std::fs::write(&target, b"previous")?;
        let error = write(
            &target,
            |file| {
                std::io::Write::write_all(file, b"partial")?;
                Err(std::io::Error::from_raw_os_error(112).into())
            },
            |_| panic!("a failed copy must not schedule any deletion"),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("host.exe"));
        assert_eq!(std::fs::read(&target)?, b"previous");
        assert_eq!(std::fs::read_dir(target.parent().unwrap())?.count(), 1);
        assert!(replace(&root.path().join("missing"), &target, |_| panic!()).is_err());
        assert_eq!(std::fs::read(&target)?, b"previous");
        Ok(())
    }

    #[test]
    #[cfg(windows)]
    fn a_locked_file_is_kept_until_it_can_be_replaced() -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;
        let root = tempfile::tempdir()?;
        let source = root.path().join("new.exe");
        let target = root.path().join("host.exe");
        std::fs::write(&source, b"next")?;
        std::fs::write(&target, b"previous")?;
        let held = File::options().read(true).share_mode(1).open(&target)?;
        assert!(replace(&source, &target, |_| panic!()).is_err());
        assert_eq!(std::fs::read(&target)?, b"previous");
        drop(held);
        replace(&source, &target, |_| panic!())?;
        assert_eq!(std::fs::read(&target)?, b"next");
        Ok(())
    }
}
