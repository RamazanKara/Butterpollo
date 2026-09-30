//! Service supervisor; the child is the Rust streaming executable in the active session.
use anyhow::{Context, Result};
use std::{
    ffi::OsString,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};
pub const NAME: &str = "ApolloService";
define_windows_service!(ffi_main, service_main);
pub fn run() -> Result<()> {
    service_dispatcher::start(NAME, ffi_main)?;
    Ok(())
}
fn service_main(_args: Vec<OsString>) {
    if let Err(e) = supervise() {
        tracing::error!(error=%e,"Windows service failed");
    }
}
fn status(state: ServiceState, checkpoint: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::SESSION_CHANGE
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(0),
        checkpoint,
        wait_hint: if checkpoint != 0 {
            Duration::from_secs(25)
        } else {
            Duration::ZERO
        },
        process_id: None,
    }
}
fn supervise() -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let changed = Arc::new(AtomicBool::new(false));
    let event_stop = stop.clone();
    let event_changed = changed.clone();
    let reporter = service_control_handler::register(NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            event_stop.store(true, Ordering::Release);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::SessionChange(_) => {
            event_changed.store(true, Ordering::Release);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    struct ReportStopped(
        windows_service::service_control_handler::ServiceStatusHandle,
        bool,
    );
    impl Drop for ReportStopped {
        fn drop(&mut self) {
            if !self.1 {
                let mut failed = status(ServiceState::Stopped, 0);
                failed.exit_code = ServiceExitCode::Win32(1067);
                let _ = self.0.set_service_status(failed);
            }
        }
    }
    let mut cleanup = ReportStopped(reporter, false);
    reporter.set_service_status(status(ServiceState::StartPending, 1))?;
    let executable = std::env::current_exe()?
        .parent()
        .context("service directory unavailable")?
        .join("butterpollo.exe");
    let config = std::path::PathBuf::from(
        std::env::var_os("PROGRAMDATA").unwrap_or_else(|| "C:\\ProgramData".into()),
    )
    .join("Butterpollo/config");
    let args = vec![
        OsString::from("--config-dir"),
        config.into_os_string(),
        OsString::from("--bind"),
        OsString::from("0.0.0.0"),
    ];
    reporter.set_service_status(status(ServiceState::Running, 0))?;
    let mut child: Option<crate::process::Process> = None;
    let mut session = u32::MAX;
    let mut failures = std::collections::VecDeque::new();
    let mut retry_at = Instant::now();
    while !stop.load(Ordering::Acquire) {
        let current =
            unsafe { windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId() };
        if (changed.swap(false, Ordering::AcqRel) || current != session) && current != session {
            if let Some(process) = child.take() {
                let _ = process.shutdown_host();
            }
            session = current;
            retry_at = Instant::now();
        }
        if let Some(process) = &child
            && let Some(code) = process.exit_code()?
        {
            child = None;
            if code == 0 {
                stop.store(true, Ordering::Release);
                break;
            }
            let now = Instant::now();
            failures.push_back(now);
            while failures
                .front()
                .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
            {
                failures.pop_front();
            }
            retry_at = now
                + if failures.len() >= 3 {
                    Duration::from_secs(30)
                } else {
                    Duration::from_secs(1)
                };
        }
        if child.is_none() && session != u32::MAX && Instant::now() >= retry_at {
            match crate::process::Process::spawn(
                &executable,
                &args,
                executable.parent(),
                crate::process::Target::SystemSession(session),
                &Default::default(),
                true,
            ) {
                Ok(process) => child = Some(process),
                Err(e) => {
                    tracing::warn!(error=%e,"service host launch failed");
                    retry_at = Instant::now() + Duration::from_secs(5);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    reporter.set_service_status(status(ServiceState::StopPending, 1))?;
    if let Some(process) = child.take() {
        let _ = process.shutdown_host();
        drop(process);
    }
    // All process and job handles are released before SCM observes STOPPED.
    reporter.set_service_status(status(ServiceState::Stopped, 0))?;
    cleanup.1 = true;
    Ok(())
}
