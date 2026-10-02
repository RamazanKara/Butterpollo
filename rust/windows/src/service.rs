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
pub const RESTART_EXIT_CODE: u32 = 75;

#[derive(Default)]
struct RestartPolicy {
    failures: std::collections::VecDeque<Instant>,
}
impl RestartPolicy {
    fn exited(&mut self, code: u32, now: Instant) -> Option<Duration> {
        match code {
            0 => None,
            RESTART_EXIT_CODE => Some(Duration::ZERO),
            _ => {
                self.failures
                    .retain(|failure| now.duration_since(*failure) < Duration::from_secs(60));
                self.failures.push_back(now);
                Some(if self.failures.len() >= 3 {
                    Duration::from_secs(30)
                } else {
                    Duration::from_secs(1)
                })
            }
        }
    }
}

fn shutdown_child(process: crate::process::Process) {
    let started = Instant::now();
    match process.shutdown_host() {
        Ok(outcome) => tracing::info!(
            pid = process.pid,
            elapsed_ms = started.elapsed().as_millis(),
            ?outcome,
            "service host stopped"
        ),
        Err(error) => {
            tracing::error!(pid=process.pid, %error, "service host shutdown failed; closing its owned job")
        }
    }
}
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
    let event_stop = stop.clone();
    let reporter = service_control_handler::register(NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            event_stop.store(true, Ordering::Release);
            ServiceControlHandlerResult::NoError
        }
        // The supervisor checks the active console session every 100 ms.
        ServiceControl::SessionChange(_) => ServiceControlHandlerResult::NoError,
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
    let mut restart = RestartPolicy::default();
    let mut retry_at = Instant::now();
    while !stop.load(Ordering::Acquire) {
        let current =
            unsafe { windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId() };
        if current != session {
            if let Some(process) = child.take() {
                shutdown_child(process);
            }
            session = current;
            restart = RestartPolicy::default();
            retry_at = Instant::now();
        }
        if let Some(process) = &child
            && let Some(code) = process.exit_code()?
        {
            child = None;
            let now = Instant::now();
            let Some(delay) = restart.exited(code, now) else {
                stop.store(true, Ordering::Release);
                break;
            };
            if code == RESTART_EXIT_CODE {
                tracing::info!("host requested a restart");
            } else {
                tracing::warn!(
                    code,
                    retry_seconds = delay.as_secs(),
                    "host exited unexpectedly"
                );
            }
            retry_at = now + delay;
        }
        if child.is_none() && session != u32::MAX && Instant::now() >= retry_at {
            match crate::process::Process::spawn_host(
                &executable,
                &args,
                executable.parent(),
                crate::process::Target::SystemSession(session),
            ) {
                Ok(process) => {
                    tracing::info!(pid = process.pid, session, "service host launched");
                    child = Some(process);
                }
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
        shutdown_child(process);
    }
    // All process and job handles are released before SCM observes STOPPED.
    reporter.set_service_status(status(ServiceState::Stopped, 0))?;
    cleanup.1 = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requested_restarts_do_not_trigger_crash_backoff_or_stop_the_service() {
        let mut policy = RestartPolicy::default();
        let now = Instant::now();
        for second in 0..10 {
            assert_eq!(
                policy.exited(RESTART_EXIT_CODE, now + Duration::from_secs(second)),
                Some(Duration::ZERO)
            );
        }
        assert!(policy.failures.is_empty());
        assert_eq!(
            policy.exited(1, now + Duration::from_secs(10)),
            Some(Duration::from_secs(1))
        );
        assert_eq!(policy.exited(0, now + Duration::from_secs(11)), None);
    }
    #[test]
    fn repeated_crashes_back_off_and_old_failures_expire() {
        let mut policy = RestartPolicy::default();
        let now = Instant::now();
        for second in 0..3 {
            let expected = if second == 2 { 30 } else { 1 };
            assert_eq!(
                policy.exited(1, now + Duration::from_secs(second)),
                Some(Duration::from_secs(expected))
            );
        }
        assert_eq!(
            policy.exited(RESTART_EXIT_CODE, now + Duration::from_secs(3)),
            Some(Duration::ZERO)
        );
        assert_eq!(policy.failures.len(), 3);
        assert_eq!(
            policy.exited(1, now + Duration::from_secs(62)),
            Some(Duration::from_secs(1))
        );
        assert_eq!(policy.failures.len(), 1);
    }
}
