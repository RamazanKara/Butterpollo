mod maintenance;
mod nvhttp;
mod process;
mod remote_display;
mod rtsp_server;
mod state;
mod stream;
mod tls;
mod web;

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
    #[arg(long)]
    assets: Option<PathBuf>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long, default_value = "127.0.0.1")]
    bind: IpAddr,
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
    #[arg(long, hide = true)]
    display_watch: Option<u32>,
    #[arg(long, hide = true)]
    open_web: Option<u16>,
}
#[tokio::main]
async fn main() -> Result<()> {
    butterpollo_windows::capture::enable_dpi_awareness();
    let args = Args::parse();
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
                &serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"displays":butterpollo_windows::capture::displays()?,"monitors":butterpollo_windows::display::monitors()?,"virtual_display_driver":butterpollo_windows::display::virtual_display_available()})
            )?
        );
        return Ok(());
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
    let directory = args.config_dir.unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
            .join("ButterpolloRust/config")
    });
    let assets = args.assets.unwrap_or_else(|| {
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("assets/web")
    });
    let h = state::Host::load(directory, assets, args.port)?;
    butterpollo_windows::crash::initialize(&h.directory)?;
    butterpollo_windows::display_recovery::initialize(&h.directory)?;
    let appender = tracing_appender::rolling::never(h.directory.join("logs"), "butterpollo.log");
    let (writer, _log_guard) = tracing_appender::non_blocking(appender);
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "butterpollo=info".into()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    let ports = h.config.read().unwrap().ports()?;
    let stop_signal = butterpollo_windows::process::StopSignal::new()?;
    let (tray, actions) = if args.no_tray {
        (None, None)
    } else {
        match butterpollo_windows::tray::Tray::new(h.assets.join("images/apollo.ico"), ports.web) {
            Ok((tray, events)) => (Some(tray), Some(events)),
            Err(e) => {
                tracing::warn!(error=%e,"tray unavailable");
                (None, None)
            }
        }
    };
    let media = stream::Media::new(h.clone(), args.bind)?;
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
        let address = (args.bind, port).into();
        tasks.spawn(tls::serve(address, router, acceptor));
    }
    tasks.spawn(rtsp_server::serve(
        (args.bind, ports.rtsp).into(),
        h.clone(),
        media,
    ));
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        web_port = ports.web,
        "Butterpollo Rust host started"
    );
    let stop = h.clone();
    let outcome: Result<()> = tokio::select! {
        signal=tokio::signal::ctrl_c()=>signal.context("waiting for shutdown"),
        task=tasks.join_next()=>{
            match task {
                Some(Ok(Err(error))) => Err(error.context("host listener failed")),
                Some(Err(error)) => Err(error.into()),
                _ => Err(anyhow::anyhow!("host listener stopped unexpectedly")),
            }
        },
        _=async move{
            while !stop.stop.load(Ordering::Acquire) {
                if stop_signal.requested(){stop.stop.store(true,Ordering::Release);}
                let finished={let mut app=stop.current_app.lock().unwrap();app.as_mut().is_some_and(|app|match app.exited(){Ok(finished)=>finished,Err(error)=>{tracing::warn!(%error,"application exit check failed");false}})};
                if finished {
                    stop.sessions.lock().unwrap().stop_role(butterpollo_core::session::Role::Stream,None);
                    stop.current_app.lock().unwrap().take();
                }
                if let Some(actions)=&actions {while let Ok(action)=actions.try_recv(){
                    use butterpollo_windows::tray::Action;
                    match action {
                        Action::Open=>{let _=butterpollo_windows::tray::open_web(ports.web);},
                        Action::StopSessions=>stop.sessions.lock().unwrap().request_stop(None),
                        Action::Restart=>{stop.restart.store(true,Ordering::Release);stop.stop.store(true,Ordering::Release);},
                        Action::Quit=>stop.stop.store(true,Ordering::Release),
                    }
                }}
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }=>Ok(())
    };
    h.stop.store(true, Ordering::Release);
    h.sessions.lock().unwrap().request_stop(None);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !h.sessions.lock().unwrap().active.is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    h.current_app.lock().unwrap().take();
    remote_display::disconnect(&h, None);
    drop(tray);
    if h.restart.load(Ordering::Acquire) {
        if butterpollo_windows::process::is_system() {
            std::process::exit(75);
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
