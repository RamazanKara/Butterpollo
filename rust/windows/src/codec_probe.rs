//! Optional Vulkan codec detection is isolated from the streaming host.
//!
//! An injected overlay can fault on a device that a short capability probe has
//! already destroyed. A failed or stalled probe must not take down AV1/HEVC.
use crate::{
    capture,
    encoder::Encoder,
    ipc::Pipe,
    process::{Process, Target},
};
use anyhow::{Context, Result, bail, ensure};
use butterpollo_core::{config::Config, rtsp::Negotiated};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

const PIPE_PREFIX: &str = r"\\.\pipe\Butterpollo.CodecProbe.";
const TIMEOUT: Duration = Duration::from_secs(15);
const FLAGS: u32 = 0x0780_0000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    output: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Reply {
    Done { flags: u32, errors: Vec<String> },
    Error { message: String },
}

pub fn pyrowave(config: &Config) -> Result<u32> {
    let (pipe, name) = Pipe::server(PIPE_PREFIX)?;
    let program = std::env::current_exe()?;
    let worker = Process::spawn(
        &program,
        &[
            "--codec-probe-worker".into(),
            name.into(),
            "--codec-probe-parent".into(),
            std::process::id().to_string().into(),
        ],
        program.parent(),
        Target::User { elevated: false },
        &BTreeMap::new(),
        true,
    )
    .context("start optional codec probe")?;
    let deadline = Instant::now() + TIMEOUT;
    let check = || -> Result<()> {
        if let Some(code) = worker.exit_code()? {
            bail!("optional codec probe exited before replying (0x{code:08x})");
        }
        ensure!(Instant::now() < deadline, "optional codec probe timed out");
        std::thread::sleep(Duration::from_millis(5));
        Ok(())
    };
    while !pipe.connected(worker.pid)? {
        check()?;
    }
    pipe.send(&Request {
        output: config.get("output_name", "").into(),
    })?;
    let reply = loop {
        if let Some(reply) = pipe.receive::<Reply>()? {
            break reply;
        }
        check()?;
    };
    pipe.send(&())?;
    ensure!(
        worker.wait(Duration::from_secs(1))? == 0,
        "optional codec probe failed during shutdown"
    );
    match reply {
        Reply::Done { flags, errors } => {
            ensure!(flags & !FLAGS == 0, "invalid optional codec flags");
            for error in errors {
                tracing::debug!(%error, "optional codec mode unavailable");
            }
            Ok(flags)
        }
        Reply::Error { message } => bail!("optional codec probe: {message}"),
    }
}

fn probe(request: &Request) -> Result<Reply> {
    let _com = capture::ComGuard::new()?;
    let image = capture::Image {
        width: 640,
        height: 480,
        stride: 2560,
        bytes: vec![128; 640 * 480 * 4],
        captured: Instant::now(),
        pixel: capture::Pixel::Bgra8,
    };
    let mut flags = 0;
    let mut errors = Vec::new();
    for (hdr, yuv444, bit) in [
        (false, false, 0x0080_0000),
        (false, true, 0x0100_0000),
        (true, false, 0x0200_0000),
        (true, true, 0x0400_0000),
    ] {
        let mode = Negotiated {
            width: 640,
            height: 480,
            fps: 30,
            bitrate_kbps: 2000,
            codec: 3,
            hdr,
            yuv444,
            ..Default::default()
        };
        let result = (|| -> Result<bool> {
            let mut encoder =
                Encoder::new_options(&mode, "auto", &request.output, &Config::default())?;
            for frame in 0..8 {
                if !encoder
                    .encode(&image, frame == 0, mode.bitrate_kbps)?
                    .is_empty()
                {
                    return Ok(true);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(false)
        })();
        match result {
            Ok(true) => flags |= bit,
            Ok(false) => errors.push(format!("HDR={hdr} 4:4:4={yuv444}: no encoded frame")),
            Err(error) => errors.push(
                format!("HDR={hdr} 4:4:4={yuv444}: {error:#}")
                    .chars()
                    .take(600)
                    .collect(),
            ),
        }
    }
    Ok(Reply::Done { flags, errors })
}

pub fn worker(name: &str, parent: u32) -> Result<()> {
    let pipe = Pipe::client(name, parent, PIPE_PREFIX)?;
    let deadline = Instant::now() + TIMEOUT;
    let request = loop {
        if let Some(request) = pipe.receive::<Request>()? {
            break request;
        }
        ensure!(
            Instant::now() < deadline,
            "optional codec request timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let reply = probe(&request).unwrap_or_else(|error| Reply::Error {
        message: format!("{error:#}").chars().take(600).collect(),
    });
    pipe.send(&reply)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while pipe.receive::<()>()?.is_none() {
        ensure!(
            Instant::now() < deadline,
            "optional codec reply was not acknowledged"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
