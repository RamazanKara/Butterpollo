//! Test helper: run a command as the installed service does, without installing it.
//! The parent must already be LocalSystem. This helper is not distributed.
use anyhow::{Context, Result, bail};
use butterpollo_windows::process::{Process, Target};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let session: u32 = args
        .next()
        .context("session ID is required")?
        .to_str()
        .context("invalid session ID")?
        .parse()?;
    let program = PathBuf::from(args.next().context("program is required")?);
    let child = Process::spawn(
        &program,
        &args.collect::<Vec<_>>(),
        None,
        Target::SystemSession(session),
        &BTreeMap::new(),
        true,
    )?;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(code) = child.exit_code()? {
            if code != 0 {
                bail!("command failed: {code}");
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("command timed out");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
