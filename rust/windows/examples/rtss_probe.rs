//! Opt-in RTSS integration probe. Query is read-only; --exercise temporarily
//! applies a fractional limit and checks restoration through the lease. It
//! requires write access to RTSS's profile; it never requests elevation.
use anyhow::{Context, Result, ensure};
use butterpollo_core::{
    config::Config,
    framegen::{Policy, Provider, Rate},
};
use butterpollo_windows::{display_recovery, limiter, rtss};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Instant,
};

fn main() -> Result<()> {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--rtss-worker") {
        ensure!(
            args.len() == 4 && args[2] == "--rtss-parent",
            "invalid helper arguments"
        );
        return rtss::worker(&args[1], args[3].parse()?);
    }
    if args.first().map(String::as_str) == Some("--display-watch") {
        ensure!(
            args.len() == 4 && args[2] == "--config-dir",
            "invalid watcher arguments"
        );
        return display_recovery::wait_and_recover(args[1].parse()?, Path::new(&args[3]));
    }
    tracing_subscriber::fmt().with_env_filter("info").init();
    let mut config = Config {
        values: BTreeMap::from([
            ("frame_limiter_enable".into(), "true".into()),
            ("frame_limiter_provider".into(), "rtss".into()),
            ("nvenc_opengl_vulkan_on_dxgi".into(), "false".into()),
            ("nvenc_latency_over_power".into(), "false".into()),
        ]),
    };
    if let Some(at) = args.iter().position(|arg| arg == "--root") {
        ensure!(at + 1 < args.len(), "--root requires a directory");
        config
            .values
            .insert("rtss_install_path".into(), args.remove(at + 1));
        args.remove(at);
    }
    let root = rtss::root(&config);
    let start = Instant::now();
    let before = rtss::query(&root)?;
    println!(
        "{}",
        serde_json::json!({"query_ms": start.elapsed().as_secs_f64()*1000., "reply": before})
    );
    if args.is_empty() {
        return Ok(());
    }
    ensure!(
        args.len() == 2 && matches!(args[0].as_str(), "--exercise" | "--exercise-recovery"),
        "usage: rtss_probe [--root RTSS_DIRECTORY] [--exercise NEW_REPORT_DIRECTORY | --exercise-recovery NEW_REPORT_DIRECTORY]"
    );
    let retry_recovery = args[0] == "--exercise-recovery";
    if retry_recovery {
        ensure!(
            std::fs::read_to_string(root.join(".butterpollo-fixture"))?
                == "isolated RTSS SDK fixture"
                && !rtss::running(&root),
            "recovery fault injection requires a stopped, isolated RTSS fixture"
        );
    }
    let directory = PathBuf::from(&args[1]);
    std::fs::create_dir(&directory).context("use a new, empty report directory")?;
    let original = rtss::read(&root)?;
    let values = rtss::properties(&original)?;
    let _writable = std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("Profiles/Global"))
        .context("the exercise needs write access to the RTSS profile")?;
    drop(_writable);
    butterpollo_core::state::write_json(
        &directory.join("before.json"),
        &serde_json::json!({"profile":original,"reply":before}),
    )?;
    display_recovery::initialize(&directory)?;
    let result = (|| -> Result<()> {
        // The watchdog can restore if the probe itself crashes.
        let policy = Policy {
            rate: Rate(59940),
            display_rate: Rate(59940),
            enabled: true,
            provider: Provider::Rtss,
            sync_limiter: 2,
            disable_vsync: false,
            smooth_motion: false,
            capture: "wgc".into(),
        };
        let lease = limiter::Lease::acquire(&directory, &config, &policy)?;
        let status = limiter::status(&config);
        ensure!(
            status["active_provider"] == "rtss",
            "limiter failed: {status}"
        );
        let during = rtss::query(&root)?;
        ensure!(
            during.values["Limit"] == Some(2997)
                && during.values["LimitDenominator"] == Some(50)
                && during.flags & 4 == 0,
            "fractional lease failed"
        );
        println!("{}", serde_json::json!({"lease":during,"status":status}));
        if retry_recovery {
            ensure!(rtss::running(&root), "the lease did not start RTSS");
            let fault = root.join("crash");
            std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&fault)?;
            drop(lease);
            std::fs::remove_file(&fault)?;
            let journal: serde_json::Value = serde_json::from_slice(&std::fs::read(
                directory.join("frame-limiter-recovery.json"),
            )?)?;
            ensure!(!journal["rtss"].is_null(), "failed restoration was lost");
            ensure!(
                rtss::running(&root),
                "RTSS ownership was lost after failure"
            );
            let lease = limiter::Lease::acquire(&directory, &config, &policy)?;
            let during = rtss::query(&root)?;
            ensure!(
                rtss::running(&root) && during.values["Limit"] == Some(2997),
                "reacquiring the lease killed its still-owned RTSS process"
            );
            println!("{}", serde_json::json!({"recovered_and_reacquired":true}));
            drop(lease);
        } else {
            drop(lease);
        }
        let after = rtss::query(&root)?;
        ensure!(
            after.values == before.values && after.flags == before.flags,
            "lease restoration mismatch"
        );
        ensure!(
            rtss::properties(&rtss::read(&root)?)? == values,
            "profile restoration mismatch"
        );
        ensure!(
            std::fs::read_to_string(directory.join("frame-limiter-recovery.json"))?
                .contains("\"rtss\": null"),
            "RTSS journal remains pending"
        );
        println!("{}", serde_json::json!({"restored":after}));
        Ok(())
    })();
    // Extra restoration attempt also covers an assertion failure in this probe.
    let recovery = limiter::recover(&directory);
    if recovery.is_ok() {
        display_recovery::external(false)?;
    }
    recovery?;
    result
}
