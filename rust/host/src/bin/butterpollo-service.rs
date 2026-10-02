fn main() -> anyhow::Result<()> {
    let directory = std::path::PathBuf::from(
        std::env::var_os("PROGRAMDATA").unwrap_or_else(|| "C:\\ProgramData".into()),
    )
    .join("Butterpollo/config/logs");
    std::fs::create_dir_all(&directory)?;
    let (writer, _guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::never(directory, "service.log"));
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .init();
    butterpollo_windows::service::run()
}
