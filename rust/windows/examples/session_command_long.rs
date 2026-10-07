//! Test helper: session_command with a deadline in seconds as the first
//! argument. The parent must already be LocalSystem. Not distributed.
//! usage: session_command_long SECONDS SESSION PROGRAM [ARGS...]
//! Build with `cargo build -p butterpollo-windows --example session_command_long
//! --target-dir target/qa` in the Rust SDK environment. Use a test-owned SYSTEM
//! scheduled task, only with no stream active, and remove the task afterward.
use anyhow::{Context, Result, bail};
use butterpollo_windows::process::{Process, Target};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let seconds: u64 = args
        .next()
        .context("deadline is required")?
        .to_str()
        .context("invalid deadline")?
        .parse()?;
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
    let deadline = Instant::now() + Duration::from_secs(seconds);
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
