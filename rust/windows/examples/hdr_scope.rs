//! Explicit, bounded HDR test scope. Never packaged or run by the host.
//! hdr_scope DISPLAY SECONDS READY.json STOP_FILE PARENT_PID
//! Restores only this monitor's HDR flag on timeout, parent exit or STOP_FILE.
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows.");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use butterpollo_windows::display::{Monitor, monitors, set_hdr};
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };
    use windows::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };

    struct Restore(Monitor);
    impl Restore {
        fn restore(&self) -> anyhow::Result<()> {
            let now = monitors()?
                .into_iter()
                .find(|m| m.device_id == self.0.device_id)
                .context("selected monitor disconnected during HDR test")?;
            set_hdr(&now, self.0.hdr_enabled)?;
            Ok(())
        }
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Err(error) = self.restore() {
                eprintln!("HDR restore failed: {error:#}");
            }
        }
    }

    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 5,
        "usage: hdr_scope DISPLAY SECONDS READY.json STOP_FILE PARENT_PID"
    );
    let seconds: u64 = args[1].parse()?;
    ensure!(
        (1..=180).contains(&seconds),
        "scope must last 1..180 seconds"
    );
    let ready = PathBuf::from(&args[2]);
    let stop = PathBuf::from(&args[3]);
    ensure!(!ready.exists() && !stop.exists(), "use fresh scope paths");
    let parent = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, args[4].parse()?) }?;
    let monitor = monitors()?
        .into_iter()
        .find(|m| m.matches(&args[0]))
        .context("explicitly selected display is not active")?;
    ensure!(
        monitor.hdr_supported,
        "selected monitor does not support HDR"
    );
    let restore = Restore(monitor);
    std::fs::write(
        ready.with_extension("before.json"),
        serde_json::to_vec_pretty(&restore.0)?,
    )?;
    if !restore.0.hdr_enabled {
        set_hdr(&restore.0, true)?;
    }
    let current = monitors()?
        .into_iter()
        .find(|m| m.device_id == restore.0.device_id)
        .context("display missing after HDR toggle")?;
    ensure!(current.hdr_enabled, "Windows did not enable HDR");
    std::fs::write(&ready, serde_json::to_vec_pretty(&current)?)?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) && !stop.exists() {
        if unsafe { WaitForSingleObject(parent, 100) } == WAIT_OBJECT_0 {
            break;
        }
    }
    unsafe {
        CloseHandle(parent)?;
    }
    restore.restore()?;
    let after = monitors()?
        .into_iter()
        .find(|m| m.device_id == restore.0.device_id)
        .context("display missing after HDR restore")?;
    ensure!(
        after.hdr_enabled == restore.0.hdr_enabled,
        "HDR flag did not restore"
    );
    std::fs::write(
        ready.with_extension("after.json"),
        serde_json::to_vec_pretty(&after)?,
    )?;
    println!("HDR scope restored");
    Ok(())
}
