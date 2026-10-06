/// The virtual display driver accepts only SYSTEM. Run as SYSTEM, for
/// example from a scheduled task, `--display-self-test REPORT` starts the
/// host's display self-test in the signed-in console session and fails when
/// a check fails; the report says which.
fn display_self_test(report: &str) -> anyhow::Result<()> {
    use butterpollo_windows::process::{Process, Target};
    anyhow::ensure!(
        butterpollo_windows::process::is_system(),
        "the display self-test must run as SYSTEM"
    );
    let host = std::env::current_exe()?.with_file_name("butterpollo.exe");
    let test = Process::spawn(
        &host,
        &["--display-self-test".into(), report.into()],
        host.parent(),
        Target::SystemSession(butterpollo_windows::process::console_session()),
        &Default::default(),
        true,
    )?;
    let code = test.wait(std::time::Duration::from_secs(300))?;
    anyhow::ensure!(code == 0, "display self-test failed ({code}); see {report}");
    Ok(())
}
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--display-self-test") {
        let report = args
            .next()
            .ok_or_else(|| anyhow::anyhow!("report path required"))?;
        return display_self_test(&report);
    }
    let directory = std::path::PathBuf::from(
        std::env::var_os("PROGRAMDATA").unwrap_or_else(|| "C:\\ProgramData".into()),
    )
    .join("Butterpollo/config/logs");
    std::fs::create_dir_all(&directory)?;
    let (writer, _guard) = tracing_appender::non_blocking(
        butterpollo_core::logfile::RotatingFile::open_default(&directory.join("service.log"))?,
    );
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .init();
    butterpollo_windows::service::run()
}
