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
/// Setup installs the virtual display driver, but an old host's uninstaller,
/// an in-app update before rc.27 or a failed driver step can leave it
/// missing, and Artemis then reports its "SudoVDA" driver as not installed.
/// When the driver is still missing a minute after the service starts (it
/// can come up after the service at boot), setup sets it up again from this
/// installation. Setup does that at most once a day per version and never
/// during a stream; the driver script stops and restarts this service.
fn repair_display_driver(install: &std::path::Path, stop: Arc<AtomicBool>) {
    use std::os::windows::process::CommandExt;
    let setup = install.join("uninstall.exe");
    if !setup.is_file() || !install.join("drivers\\display\\install.ps1").is_file() {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..12 {
            if stop.load(Ordering::Acquire) || crate::display::virtual_display_available() {
                return;
            }
            std::thread::sleep(Duration::from_secs(5));
        }
        let status = crate::display::virtual_display_status();
        tracing::warn!(
            reason = status["reason"].as_str().unwrap_or(""),
            "the virtual display driver is missing; setting it up again"
        );
        // Not a child that dies with the service: the script stops it.
        if let Err(error) = std::process::Command::new(&setup)
            .arg("--repair-drivers")
            .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0)
            .spawn()
        {
            tracing::warn!(%error, "the driver repair could not start");
        }
    });
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
    let config = butterpollo_core::paths::installed_profile();
    // Interfaces come from bind_address and address_family (IPv4 by default).
    let args = vec![
        OsString::from("--config-dir"),
        config.clone().into_os_string(),
    ];
    reporter.set_service_status(status(ServiceState::Running, 0))?;
    // An update that power loss or a crash interrupted is rolled back before
    // a host starts from a mix of two versions.
    if let Some(install) = executable.parent() {
        match butterpollo_core::update_recovery::recover(&config, install) {
            Ok(Some(outcome)) => tracing::warn!(outcome, "interrupted update rolled back"),
            Ok(None) => {}
            // The host still starts: it serves the console that reports the
            // failure, and the next start or setup tries again. Stopping the
            // service here would leave no host at all, even for an unreadable
            // record.
            Err(error) => tracing::error!(
                error = %format!("{error:#}"),
                "rolling back an interrupted update failed"
            ),
        }
    }
    if let Some(install) = executable.parent() {
        repair_display_driver(install, stop.clone());
    }
    let mut child: Option<crate::process::Process> = None;
    let mut session = u32::MAX;
    let mut restart = RestartPolicy::default();
    let mut retry_at = Instant::now();
    while !stop.load(Ordering::Acquire) {
        // SAFETY: WTSGetActiveConsoleSessionId takes no arguments and has no preconditions.
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
        // A stop can arrive while a host shuts down above; a new host would
        // only delay it.
        if child.is_none()
            && session != u32::MAX
            && Instant::now() >= retry_at
            && !stop.load(Ordering::Acquire)
        {
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
