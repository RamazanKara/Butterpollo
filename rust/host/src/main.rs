mod console;
mod display_session;
mod lossless;
mod maintenance;
mod network;
mod nvhttp;
mod playnite;
mod process;
mod pyrowave_send;
mod remote_display;
mod rtsp_server;
mod runtime;
mod state;
mod steam;
mod stream;
mod tls;
mod updater;
mod web;
mod web_sessions;

use anyhow::{Context, Result};
use clap::Parser;
use std::{
    net::IpAddr,
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(version, about = "Butterpollo's Rust Moonlight streaming host")]
struct Args {
    #[arg(long)]
    config_dir: Option<PathBuf>,
    /// Copy a Vibepollo/Apollo profile into an empty --config-dir, then exit.
    #[arg(long, requires = "config_dir")]
    import_config: Option<PathBuf>,
    #[arg(long)]
    assets: Option<PathBuf>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    bind: Option<IpAddr>,
    #[arg(long)]
    diagnostics: bool,
    #[arg(long)]
    capture_smoke: bool,
    #[arg(long)]
    encoder_smoke: Option<String>,
    #[arg(long)]
    encoder_output: Option<PathBuf>,
    #[arg(long, default_value = "wgc")]
    capture: String,
    #[arg(long)]
    hdr: bool,
    #[arg(long, default_value = "h264")]
    codec: String,
    #[arg(long)]
    virtual_display_smoke: bool,
    #[arg(long)]
    no_tray: bool,
    /// Set the web console's username and password, then exit. Without
    /// --config-dir this changes the installed host's profile.
    #[arg(long, num_args = 2, value_names = ["USERNAME", "PASSWORD"])]
    creds: Option<Vec<String>>,
    #[arg(long, hide = true)]
    service_stop_source: Option<String>,
    #[arg(long, hide = true)]
    display_watch: Option<u32>,
    #[arg(long, hide = true)]
    crash_reporter: bool,
    /// Run the display self-test and write its report; see
    /// butterpollo-service.exe --display-self-test.
    #[arg(long, hide = true)]
    display_self_test: Option<PathBuf>,
    #[arg(long, hide = true, requires = "rtss_parent")]
    rtss_worker: Option<String>,
    #[arg(long, hide = true, requires = "rtss_worker")]
    rtss_parent: Option<u32>,
    #[arg(long, hide = true, requires = "codec_probe_parent")]
    codec_probe_worker: Option<String>,
    #[arg(long, hide = true, requires = "codec_probe_worker")]
    codec_probe_parent: Option<u32>,
    #[arg(long, hide = true)]
    open_web: Option<u16>,
    #[arg(long, hide = true, requires = "wgc_parent")]
    wgc_worker: Option<String>,
    #[arg(long, hide = true, requires = "wgc_worker")]
    wgc_parent: Option<u32>,
}
#[tokio::main]
async fn main() -> Result<()> {
    butterpollo_windows::capture::enable_dpi_awareness();
    let args = Args::parse();
    if args.crash_reporter {
        return butterpollo_windows::crash::reporter();
    }
    if let Some(pipe) = &args.codec_probe_worker {
        return butterpollo_windows::codec_probe::worker(pipe, args.codec_probe_parent.unwrap());
    }
    if let Some(pipe) = &args.wgc_worker {
        return butterpollo_windows::capture::run_wgc_worker(pipe, args.wgc_parent.unwrap());
    }
    if let Some(source) = &args.import_config {
        // Setup keeps this output in its log: what was left out, and why.
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .init();
        return butterpollo_core::migration::import(source, args.config_dir.as_ref().unwrap());
    }
    if let Some(pipe) = &args.rtss_worker {
        return butterpollo_windows::rtss::worker(pipe, args.rtss_parent.unwrap());
    }
    if let Some(port) = args.open_web {
        return butterpollo_windows::tray::open_web(port);
    }
    if let Some(pid) = args.display_watch {
        return butterpollo_windows::display_recovery::wait_and_recover(
            pid,
            &args
                .config_dir
                .context("display watcher requires a config directory")?,
        );
    }
    if args.diagnostics {
        let _com = butterpollo_windows::capture::ComGuard::new()?;
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"displays":butterpollo_windows::capture::displays()?,"monitors":butterpollo_windows::display::monitors()?,"virtual_display_driver":butterpollo_windows::display::virtual_display_available(),"virtual_display_status":butterpollo_windows::display::virtual_display_status(),"firewall":std::env::current_exe().map_err(anyhow::Error::from).and_then(|exe| butterpollo_windows::firewall::problems(&exe)).unwrap_or_else(|error| vec![format!("firewall rules could not be read: {error:#}")])})
            )?
        );
        return Ok(());
    }
    if let Some(report) = &args.display_self_test {
        let _com = butterpollo_windows::capture::ComGuard::new()?;
        let passed = butterpollo_windows::display::self_test::run(report)?;
        std::process::exit(if passed { 0 } else { 1 });
    }
    if args.virtual_display_smoke {
        let _com = butterpollo_windows::capture::ComGuard::new()?;
        let display = butterpollo_windows::display::VirtualDisplay::create(
            &uuid::Uuid::new_v4().to_string(),
            640,
            480,
            30,
        )?;
        println!("Created Rust virtual display {}", display.name);
        drop(display);
        println!("Virtual display lease removed");
        return Ok(());
    }
    if args.capture_smoke || args.encoder_smoke.is_some() {
        return smoke(&args);
    }
    let assets = args.assets.clone().unwrap_or_else(|| {
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("assets/web")
    });
    if let Some(creds) = &args.creds {
        // The service's profile, unless another one is named.
        let directory = args.config_dir.clone().unwrap_or_else(|| {
            let installed = PathBuf::from(std::env::var_os("PROGRAMDATA").unwrap_or_default())
                .join("Butterpollo/config");
            if installed.join("sunshine.conf").is_file() {
                installed
            } else {
                default_directory()
            }
        });
        let h = state::Host::load(directory, assets, args.port)?;
        let credentials = butterpollo_core::state::Credentials::new(creds[0].clone(), &creds[1])?;
        // As with a password change in the console, signed-in browsers sign
        // in again.
        h.save_web_sessions(&Default::default())?;
        h.save_credentials(&credentials)
            .context("saving the credentials (an installed host needs an administrator)")?;
        println!(
            "Saved the web console credentials in {}. Restart Butterpollo if it is running.",
            h.directory.display()
        );
        return Ok(());
    }
    let supervised = args.service_stop_source.is_some();
    let stop_signal =
        butterpollo_windows::process::StopSignal::new(args.service_stop_source.as_deref())?;
    let directory = args.config_dir.unwrap_or_else(default_directory);
    let h = state::Host::load(directory, assets, args.port)?;
    butterpollo_windows::crash::initialize(&h.directory)?;
    let display_recovery = butterpollo_windows::display_recovery::initialize(&h.directory)?;
    let log_path = h
        .config
        .read()
        .unwrap()
        .path("log_path", &h.directory, "logs/butterpollo.log");
    let open = |path: &std::path::Path| -> Result<_> {
        std::fs::create_dir_all(path.parent().context("log directory missing")?)?;
        Ok(butterpollo_core::logfile::RotatingFile::open_default(path)?)
    };
    // A log_path that cannot be used (a folder, a missing drive) falls back to
    // the default file instead of keeping the host, and its console, down.
    let default_log = h.directory.join("logs/butterpollo.log");
    let (appender, unusable_log) = match open(&log_path) {
        Ok(appender) => (appender, None),
        Err(error) if log_path != default_log => (open(&default_log)?, Some((log_path, error))),
        Err(error) => return Err(error),
    };
    let log_level = h.config.read().unwrap().log_level();
    let (writer, _log_guard) = tracing_appender::non_blocking(appender);
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| log_level.into()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    if let Some((path, error)) = unusable_log {
        tracing::warn!(
            path = %path.display(),
            error = format!("{error:#}"),
            "log_path cannot be used; logging to the default file"
        );
    }
    if let Some(error) = display_recovery {
        tracing::warn!(
            error = format!("{error:#}"),
            "display settings from an interrupted stream were not all restored"
        );
    }
    // The service's keys: SYSTEM and Administrators only, also after a
    // profile was copied in or restored from a backup.
    let credentials = h.directory.join("credentials");
    if butterpollo_windows::process::is_system()
        && credentials.is_dir()
        && let Err(error) = butterpollo_windows::process::restrict_to_administrators(&credentials)
    {
        tracing::warn!(%error, "the credentials folder permissions could not be repaired");
    }
    let ports = h.config.read().unwrap().ports()?;
    if let Err(error) = butterpollo_windows::vulkan::reconcile(
        h.config.read().unwrap().boolean("vulkan_hdr_layer", true),
    ) {
        tracing::warn!(%error, "Vulkan HDR registration could not be reconciled");
    }
    let bind = network::bind_address(&h.config.read().unwrap(), args.bind)?;
    let discovery = match network::Discovery::start(&h.config.read().unwrap(), bind) {
        Ok(discovery) => discovery,
        Err(error) => {
            tracing::warn!(%error, "Moonlight discovery unavailable");
            None
        }
    };
    let port_forward = network::port_forward(h.clone(), bind);
    if let Err(error) = butterpollo_windows::display::configure_permanent(&h.config.read().unwrap())
    {
        tracing::warn!(%error, "configured permanent virtual displays could not be applied");
    }
    let (tray, actions) = if args.no_tray || !h.config.read().unwrap().boolean("system_tray", true)
    {
        (None, None)
    } else {
        let icon = h.assets.parent().unwrap_or(&h.assets).join("apollo.ico");
        let icon = if icon.is_file() {
            icon
        } else {
            h.assets.join("images/apollo.ico")
        };
        match butterpollo_windows::tray::Tray::new_options(
            icon,
            ports.web,
            h.config
                .read()
                .unwrap()
                .boolean("hide_tray_controls", false),
        ) {
            Ok((tray, events)) => (Some(tray), Some(events)),
            Err(e) => {
                tracing::warn!(error=%e,"tray unavailable");
                (None, None)
            }
        }
    };
    let media = stream::Media::new(h.clone(), bind)?;
    h.probe_codecs();
    let mut tasks = tokio::task::JoinSet::new();
    for (port, https, web) in [
        (ports.http, false, false),
        (ports.https, true, false),
        (ports.web, true, true),
    ] {
        let router = if web {
            web::router(h.clone())
        } else {
            nvhttp::router(h.clone(), https)
        };
        let acceptor = if https {
            Some(tls::acceptor(&h.identity, !web)?)
        } else {
            None
        };
        let address = (bind, port).into();
        tasks.spawn(tls::serve(address, router, acceptor));
    }
    tasks.spawn(rtsp_server::serve(
        (bind, ports.rtsp).into(),
        h.clone(),
        media,
    ));
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        web_port = ports.web,
        "Butterpollo Rust host started"
    );
    // A block rule, left when Windows' "allow access" prompt was dismissed,
    // overrides the installer's allow rule: say so rather than leave users to
    // open ports by hand. Only a host listening on the network is affected.
    if bind.is_unspecified() {
        std::thread::spawn(|| {
            let Ok(program) = std::env::current_exe() else {
                return;
            };
            match butterpollo_windows::firewall::problems(&program) {
                Ok(problems) => {
                    for problem in problems {
                        tracing::warn!(program = %program.display(), "{problem}");
                    }
                }
                Err(error) => tracing::debug!(%error, "Windows Firewall rules could not be read"),
            }
        });
    }
    let outcome: Result<()> = tokio::select! {
        signal=tokio::signal::ctrl_c()=>signal.context("waiting for shutdown"),
        task=tasks.join_next()=>{
            match task {
                Some(Ok(Err(error))) => Err(error.context("host listener failed")),
                Some(Err(error)) => Err(error.into()),
                _ => Err(anyhow::anyhow!("host listener stopped unexpectedly")),
            }
        },
        _=runtime::maintain(h.clone(), stop_signal, actions, ports.web)=>Ok(())
    };
    h.stop.store(true, Ordering::Release);
    h.sessions.lock().unwrap().request_stop(None);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !h.sessions.lock().unwrap().active.is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    h.stop_app();
    remote_display::disconnect(&h, None);
    drop(discovery);
    if let Some(mut task) = port_forward
        && tokio::time::timeout(Duration::from_secs(10), &mut task)
            .await
            .is_err()
    {
        task.abort();
    }
    drop(tray);
    if h.restart.load(Ordering::Acquire) {
        if supervised {
            drop(_log_guard);
            std::process::exit(butterpollo_windows::service::RESTART_EXIT_CODE as i32);
        } else {
            use std::os::windows::process::CommandExt;
            std::process::Command::new(std::env::current_exe()?)
                .args(std::env::args_os().skip(1))
                .creation_flags(0x08000000)
                .spawn()?;
        }
    }
    outcome
}
/// A portable host's profile.
fn default_directory() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
        .join("ButterpolloRust/config")
}
fn smoke(args: &Args) -> Result<()> {
    let _com = butterpollo_windows::capture::ComGuard::new()?;
    let _priority = butterpollo_windows::capture::Priority::new();
    let image = if args.capture_smoke {
        let mut capture =
            butterpollo_windows::capture::Capture::new_format("", &args.capture, args.hdr)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(image) = capture.next_frame()? {
                break image;
            }
            if Instant::now() > deadline {
                anyhow::bail!("capture did not deliver a frame");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    } else {
        butterpollo_windows::capture::Image {
            width: 128,
            height: 128,
            stride: 512,
            bytes: vec![128; 128 * 128 * 4],
            captured: Instant::now(),
            pixel: butterpollo_windows::capture::Pixel::Bgra8,
        }
    };
    let mut output = serde_json::json!({"capture":args.capture,"width":image.width,"height":image.height,"bytes":image.bytes.len()});
    if let Some(name) = &args.encoder_smoke {
        let cfg = butterpollo_core::rtsp::Negotiated {
            width: if args.codec == "h264" { 128 } else { 256 },
            height: if args.codec == "h264" { 128 } else { 256 },
            hdr: args.hdr,
            codec: match args.codec.as_str() {
                "hevc" | "h265" => 1,
                "av1" => 2,
                "pyrowave" => 3,
                _ => 0,
            },
            ..Default::default()
        };
        let mut encoder = butterpollo_windows::encoder::Encoder::new(&cfg, name, "")?;
        let mut packets = 0;
        let mut bytes = 0;
        for i in 0..30 {
            for frame in encoder.encode(&image, i == 0, cfg.bitrate_kbps)? {
                if packets == 0
                    && let Some(path) = &args.encoder_output
                {
                    std::fs::write(path, &frame.bytes)?;
                }
                packets += 1;
                bytes += frame.bytes.len();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if packets == 0 {
            anyhow::bail!("encoder delivered no packets");
        }
        output["encoder"] = serde_json::json!(name);
        output["encoded_frames"] = serde_json::json!(packets);
        output["encoded_bytes"] = serde_json::json!(bytes);
    }
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
