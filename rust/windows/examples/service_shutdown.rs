//! A disposable child validates the actual supervisor stop channel without
//! opening capture, changing displays, or touching a host profile.
use anyhow::{Context, Result, bail};
use butterpollo_windows::process::{HostShutdown, Process, StopSignal, Target};
use serde_json::json;
use std::{
    ffi::OsString,
    path::PathBuf,
    time::{Duration, Instant},
};
use windows::Win32::System::{RemoteDesktop::ProcessIdToSessionId, Threading::GetCurrentProcessId};

fn session_id() -> Result<u32> {
    let mut session = 0;
    unsafe {
        ProcessIdToSessionId(GetCurrentProcessId(), &mut session)?;
    }
    Ok(session)
}
fn argument(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let report = PathBuf::from(argument(&args, "--report").context("--report is required")?);
    let child_report = report.with_extension("child.json");
    if let Some(source) = argument(&args, "--service-stop-source") {
        let signal = StopSignal::new(Some(&source))?;
        let mut result =
            json!({"pid":std::process::id(),"session":session_id()?,"shutdown_requested":false});
        butterpollo_core::state::write_json(&child_report, &result)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !signal.requested() {
            if Instant::now() >= deadline {
                bail!("supervisor did not request shutdown");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        result["shutdown_requested"] = json!(true);
        butterpollo_core::state::write_json(&child_report, &result)?;
        return Ok(());
    }
    let target_session = argument(&args, "--system-session")
        .map(|value| value.parse::<u32>())
        .transpose()?;
    let target = match target_session {
        Some(session) => Target::SystemSession(session),
        None => Target::User { elevated: false },
    };
    let child_args: Vec<OsString> = vec!["--report".into(), report.as_os_str().to_owned()];
    let child = Process::spawn_host(&std::env::current_exe()?, &child_args, None, target)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(value) = std::fs::read(&child_report)
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&value)
            && value["pid"].as_u64() == Some(child.pid as u64)
        {
            break;
        }
        if let Some(code) = child.exit_code()? {
            bail!("child exited before readiness: {code}");
        }
        if Instant::now() >= deadline {
            bail!("child did not become ready");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let started = Instant::now();
    let outcome = child.shutdown_host()?;
    let elapsed = started.elapsed();
    if outcome != HostShutdown::Graceful(0) {
        bail!("child needed forced shutdown: {outcome:?}");
    }
    let result: serde_json::Value = serde_json::from_slice(&std::fs::read(&child_report)?)?;
    if result["shutdown_requested"] != true {
        bail!("child did not observe the supervisor event");
    }
    if let Some(session) = target_session
        && result["session"].as_u64() != Some(session as u64)
    {
        bail!("child entered the wrong Windows session");
    }
    let report_value = json!({"status":"pass","parent_session":session_id()?,"child_session":result["session"],"shutdown_ms":elapsed.as_secs_f64()*1000.,"graceful":true});
    butterpollo_core::state::write_json(&report, &report_value)?;
    println!("{report_value}");
    Ok(())
}
