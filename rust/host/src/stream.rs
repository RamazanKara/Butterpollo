use crate::state::Shared;
mod capture;
use anyhow::{Context, Result};
use butterpollo_core::{
    config::Config,
    crypto, input,
    packet::{AudioPacketizer, VideoPacketizer},
    session::{Role, Session},
};
pub(crate) const RTX_KEYS: &[&str] = &[
    "rtx_hdr",
    "rtx_hdr_sdr_brightness",
    "rtx_hdr_contrast",
    "rtx_hdr_saturation",
    "rtx_hdr_middle_gray",
    "rtx_hdr_peak_brightness",
];
/// How often completed encoder output is collected while a frame is in flight.
const OUTPUT_POLL: Duration = Duration::from_micros(100);
/// Time for streams to release a lost capture device before it is re-created.
/// Streams notice the reset within one 50 ms frame wait.
const RELEASE_WAIT: Duration = Duration::from_millis(150);
fn rtx_parameters(config: &Config) -> [u32; 4] {
    let peak = config
        .integer("rtx_hdr_peak_brightness", 1000)
        .clamp(400, 2000) as u32;
    let scale = (peak as f32 / 1000.).max(1.);
    [
        (config.integer("rtx_hdr_contrast", 0) + 100).clamp(0, 200) as u32,
        (config.integer("rtx_hdr_saturation", 0) + 100).clamp(0, 200) as u32,
        (config.integer("rtx_hdr_middle_gray", 50).clamp(10, 100) as f32 / scale)
            .round()
            .clamp(10., 100.) as u32,
        peak.min(1000),
    ]
}
fn rtx_enabled(config: &Config) -> bool {
    config.boolean("rtx_hdr", false)
        && config.boolean(&butterpollo_core::rtx_policy::marker("rtx_hdr"), false)
}
fn truehdr_filter(
    image: &GpuImage,
    config: &Config,
) -> Option<butterpollo_windows::truehdr::Filter> {
    if image.pixel != butterpollo_windows::capture::Pixel::Bgra8 {
        return None;
    }
    match butterpollo_windows::truehdr::Filter::new_gpu(image, rtx_parameters(config)) {
        Ok(filter) => Some(filter),
        Err(error) => {
            tracing::warn!(%error, "TrueHDR unavailable; using neutral SDR-to-PQ conversion");
            None
        }
    }
}

pub(crate) fn effective_config(
    h: &Shared,
    launch: &butterpollo_core::session::Launch,
) -> Result<Config> {
    let mut config = h.config.read().unwrap().clone();
    if let Some(overrides) = launch
        .client
        .extra
        .get("config_overrides")
        .and_then(serde_json::Value::as_object)
    {
        apply_overrides(&mut config, overrides)?;
    }
    if let Some(value) = launch
        .client
        .extra
        .get("prefer_10bit_sdr")
        .filter(|v| !v.is_null() && v.as_str() != Some(""))
    {
        config.values.insert(
            "prefer_sdr_10bit".into(),
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string()),
        );
    }
    let inherited = config.clone();
    let app = h
        .apps
        .read()
        .unwrap()
        .iter()
        .find(|app| app.id() == launch.app_id || app.aliases.contains(&launch.app_id))
        .cloned();
    let app_uuid = app
        .as_ref()
        .and_then(|app| app.extra.get("uuid"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if let Some(app) = &app {
        for (source, target) in [
            ("gamepad", "gamepad"),
            ("dd-configuration-option", "dd_configuration_option"),
            ("prefer-10bit-sdr", "prefer_sdr_10bit"),
            ("rtx-hdr", "rtx_hdr"),
            ("rtx-hdr-sdr-brightness", "rtx_hdr_sdr_brightness"),
            ("rtx-hdr-contrast", "rtx_hdr_contrast"),
            ("rtx-hdr-saturation", "rtx_hdr_saturation"),
            ("rtx-hdr-middle-gray", "rtx_hdr_middle_gray"),
            ("rtx-hdr-peak-brightness", "rtx_hdr_peak_brightness"),
        ] {
            if let Some(value) = app
                .extra
                .get(source)
                .filter(|value| !value.is_null() && value.as_str() != Some(""))
            {
                config.values.insert(
                    target.into(),
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string()),
                );
                if RTX_KEYS.contains(&target) {
                    config
                        .values
                        .insert(butterpollo_core::rtx_policy::marker(target), "true".into());
                }
            }
        }
    }
    if let Some(overrides) = app
        .as_ref()
        .and_then(|app| app.extra.get("config-overrides"))
        .and_then(serde_json::Value::as_object)
    {
        apply_overrides(&mut config, overrides)?;
    }
    if let Some((uuid, values)) = h.live_rtx.lock().unwrap().as_ref()
        && Some(uuid) == app_uuid.as_ref()
    {
        for &key in RTX_KEYS {
            match inherited.values.get(key) {
                Some(value) => {
                    config.values.insert(key.into(), value.clone());
                }
                None => {
                    config.values.remove(key);
                }
            }
            let marker = butterpollo_core::rtx_policy::marker(key);
            if let Some(value) = inherited.values.get(&marker) {
                config.values.insert(marker, value.clone());
            } else {
                config.values.remove(&marker);
            }
        }
        apply_overrides(&mut config, values)?;
    }
    if !config.boolean(
        &butterpollo_core::rtx_policy::marker("rtx_hdr_peak_brightness"),
        false,
    ) && let Some(selection) = launch
        .client
        .extra
        .get("hdr_profile")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        match butterpollo_windows::hdr_profile::peak_luminance(selection) {
            Ok(Some(peak)) => {
                config.values.insert(
                    "rtx_hdr_peak_brightness".into(),
                    peak.clamp(400, 2000).to_string(),
                );
                config.values.insert(
                    butterpollo_core::rtx_policy::marker("rtx_hdr_peak_brightness"),
                    "true".into(),
                );
            }
            Ok(None) => tracing::debug!(%selection, "HDR calibration has no MHC2 peak"),
            Err(error) => tracing::debug!(%error, %selection, "HDR calibration peak unavailable"),
        }
    }
    if let Some(value) = launch.options.get("virtualDisplay") {
        config.values.insert(
            "virtual_display_mode".into(),
            if value == "0" {
                "disabled"
            } else {
                "per_client"
            }
            .into(),
        );
    }
    Ok(config)
}
fn apply_overrides(
    config: &mut Config,
    overrides: &serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    let overrides: serde_json::Map<String, serde_json::Value> = overrides
        .iter()
        .filter(|(_, v)| v.as_str() != Some(""))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    config.update(&overrides)?;
    for (key, value) in overrides {
        if RTX_KEYS.contains(&key.as_str()) {
            let marker = butterpollo_core::rtx_policy::marker(&key);
            if value.is_null() {
                config.values.remove(&marker);
            } else {
                config.values.insert(marker, "true".into());
            }
        }
    }
    Ok(())
}
use butterpollo_windows::{
    audio::{Loopback, Opus},
    capture::{Capture, ComGuard, GpuImage, Priority},
    encoder::Encoder,
    input::Injector,
};
use rusty_enet::{Event, Host, HostSettings, Packet, PacketKind, PeerID};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr, UdpSocket},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

type Latest = capture::Latest<GpuImage>;
struct Source {
    latest: Arc<Latest>,
    grid: Arc<Mutex<butterpollo_windows::capture::ClaimGrid>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Drop for Source {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}
impl std::ops::Deref for Source {
    type Target = Latest;
    fn deref(&self) -> &Latest {
        &self.latest
    }
}
pub struct Media {
    video: Arc<UdpSocket>,
    audio: Arc<UdpSocket>,
    peers: Mutex<HashMap<(String, bool), SocketAddr>>,
    captures: Mutex<HashMap<String, Weak<Source>>>,
    control_port: u16,
    bind: IpAddr,
}
impl Media {
    pub fn new(h: Shared, bind: IpAddr) -> Result<Arc<Self>> {
        let ports = h.config.read().unwrap().ports()?;
        let video = crate::network::udp((bind, ports.video).into())?;
        let audio = crate::network::udp((bind, ports.audio).into())?;
        butterpollo_windows::net::configure_udp(&video)?;
        butterpollo_windows::net::configure_udp(&audio)?;
        video.set_nonblocking(true)?;
        audio.set_nonblocking(true)?;
        let m = Arc::new(Self {
            video: Arc::new(video),
            audio: Arc::new(audio),
            peers: Mutex::new(HashMap::new()),
            captures: Mutex::new(HashMap::new()),
            control_port: ports.control,
            bind,
        });
        for is_audio in [false, true] {
            let socket = if is_audio {
                m.audio.clone()
            } else {
                m.video.clone()
            };
            let m = m.clone();
            let h = h.clone();
            thread::Builder::new()
                .name(if is_audio { "audio-ping" } else { "video-ping" }.into())
                .spawn(move || {
                    let mut b = [0; 2048];
                    while !h.stop.load(Ordering::Acquire) {
                        match socket.recv_from(&mut b) {
                            Ok((n, peer)) => {
                                let sessions = h.sessions.lock().unwrap();
                                for s in sessions.active.values() {
                                    if s.launch.peer != peer.ip().to_canonical() {
                                        continue;
                                    }
                                    let valid = (n >= 16
                                        && crypto::equal(&b[..16], s.launch.ping.as_bytes()))
                                        || (n == 4
                                            && &b[..4] == b"PING"
                                            && sessions
                                                .active
                                                .values()
                                                .filter(|s| {
                                                    s.launch.peer == peer.ip().to_canonical()
                                                })
                                                .count()
                                                == 1);
                                    if valid {
                                        m.peers
                                            .lock()
                                            .unwrap()
                                            .insert((s.launch.id.clone(), is_audio), peer);
                                        break;
                                    }
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(1))
                            }
                            Err(e) => {
                                tracing::warn!(error=%e,"media ping receive failed");
                                thread::sleep(Duration::from_millis(10));
                            }
                        }
                    }
                })?;
        }
        let control = m.clone();
        thread::Builder::new()
            .name("control".into())
            .spawn(move || {
                if let Err(e) = control.control(h) {
                    tracing::error!(error=%e,"control worker stopped");
                }
            })?;
        Ok(m)
    }
    fn capture(
        &self,
        kind: &str,
        hdr: bool,
        config: &Config,
        rate: butterpollo_core::framegen::Rate,
        phase: &str,
        prepared: Arc<crate::display_session::Ready>,
    ) -> Result<Arc<Source>> {
        let output = prepared.output();
        let aligned = config.boolean("wgc_slot_aligned_publish", false);
        let key = format!(
            "{kind}:{output}:{hdr}:{}:{}:{}",
            config.get("adapter_name", ""),
            config.get("adapter_pnp_id", ""),
            if aligned { phase } else { "" },
        );
        let mut captures = self.captures.lock().unwrap();
        captures.retain(|_, source| source.strong_count() > 0);
        if let Some(existing) = captures.get(&key).and_then(Weak::upgrade)
            && existing.check().is_ok()
        {
            return Ok(existing);
        }
        let latest = Arc::new(Latest::new());
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = latest.clone();
        let grid = Arc::new(Mutex::new(butterpollo_windows::capture::ClaimGrid {
            anchor: Instant::now(),
            period: rate.period(),
        }));
        let worker_grid = grid.clone();
        let kind = kind.to_owned();
        let capture_config = config.clone();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                let result = (|| -> Result<()> {
                    let _com = ComGuard::new()?;
                    let _priority = Priority::new();
                    let timer = butterpollo_windows::timing::Timer::new()?;
                    let mut target = prepared.capture_target();
                    let mut capture =
                        match Capture::new_options(&target.0, &kind, hdr, &capture_config) {
                            Ok(c) => c,
                            Err(e) => {
                                let _ = started_tx.send(Err(e.to_string()));
                                return Ok(());
                            }
                        };
                    capture.set_claim_grid(worker_grid.clone(), aligned);
                    tracing::info!(requested=%kind, backend=capture.backend(), output=%target.0, "capture backend opened");
                    // Re-create the capture on a new device. Windows keeps handing
                    // out the stale adapter while anything holds the old device, so
                    // every duplication made then loses access at once (a display
                    // arriving for another client does this). The old capture is
                    // dropped and consumers are given time to release their frames
                    // and encoders before the new device is made.
                    let reopen = |lost: Capture, target: &(String, u64)| -> Result<Capture> {
                        drop(lost);
                        worker.publish(None)?;
                        let deadline = Instant::now() + Duration::from_secs(30);
                        loop {
                            thread::sleep(RELEASE_WAIT);
                            if worker_stop.load(Ordering::Acquire) {
                                anyhow::bail!("capture stopped while recovering");
                            }
                            let next = prepared.capture_target();
                            match Capture::new_options(&next.0, &kind, hdr, &capture_config) {
                                Ok(recovered) => {
                                    if next != *target {
                                        tracing::info!(output = %next.0, "capture moved to the recreated display");
                                    }
                                    return Ok(recovered);
                                }
                                Err(error) if Instant::now() >= deadline => return Err(error),
                                Err(_) => {}
                            }
                        }
                    };
                    let _ = started_tx.send(Ok(()));
                    let mut check_target = Instant::now();
                    let poll_interval = Duration::from_micros(
                        capture_config.integer("capture_poll_interval_us", 500).clamp(100, 1000) as u64,
                    );
                    while !worker_stop.load(Ordering::Acquire) {
                        if Instant::now() >= check_target {
                            check_target = Instant::now() + Duration::from_millis(100);
                            let next = prepared.capture_target();
                            if next != target {
                                let lost = std::mem::replace(&mut capture, Capture::Closed);
                                capture = match reopen(lost, &next) {
                                    Ok(capture) => capture,
                                    Err(_) if worker_stop.load(Ordering::Acquire) => return Ok(()),
                                    Err(error) => return Err(error),
                                };
                                capture.set_claim_grid(worker_grid.clone(), aligned);
                                target = prepared.capture_target();
                            }
                        }
                        match capture.next_gpu() {
                            Ok(Some(image)) => {
                                let captured = image.captured;
                                worker.publish_captured(Arc::new(image), captured)?;
                            }
                            Ok(None) => {
                                let interval = if capture_config.boolean("capture_predictive_poll", false)
                                    && capture.backend() == "ddx" {
                                    worker.poll_interval(poll_interval)
                                } else { poll_interval };
                                let deadline = Instant::now() + interval;
                                timer.until(capture.publication_deadline().unwrap_or(deadline).min(deadline));
                            }
                            Err(e) => {
                                tracing::warn!(error=%e, output=%target.0, backend=capture.backend(), "capture restarting");
                                let lost = std::mem::replace(&mut capture, Capture::Closed);
                                capture = match reopen(lost, &target) {
                                    Ok(capture) => capture,
                                    Err(_) if worker_stop.load(Ordering::Acquire) => return Ok(()),
                                    Err(error) => return Err(error),
                                };
                                capture.set_claim_grid(worker_grid.clone(), aligned);
                                target = prepared.capture_target();
                            }
                        }
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    let _ = worker.fail(e.to_string());
                    tracing::error!(error=%e,"capture worker stopped");
                }
            })?;
        let latest = Arc::new(Source {
            latest,
            grid,
            stop,
            thread: Some(thread),
        });
        started_rx
            .recv_timeout(Duration::from_secs(10))?
            .map_err(|e| anyhow::anyhow!(e))?;
        captures.insert(key, Arc::downgrade(&latest));
        Ok(latest)
    }
    pub fn start(self: &Arc<Self>, h: Shared, s: Arc<Session>) {
        let m = self.clone();
        let result = thread::Builder::new()
            .name("session".into())
            .spawn(move || {
                tracing::info!(client=%s.launch.client.name,"CLIENT CONNECTED");
                {
                    let mut paired = h.paired.write().unwrap();
                    if let Some(client) = paired
                        .clients
                        .iter_mut()
                        .find(|c| c.uuid == s.launch.client.uuid)
                    {
                        client.extra.insert(
                            "last_seen".into(),
                            serde_json::json!(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs()
                            ),
                        );
                        if let Err(error) = paired.save(&h.paired_path) {
                            tracing::warn!(%error,"client last-seen persistence failed");
                        }
                    }
                }
                let result = (|| -> Result<()> {
                    let _com = ComGuard::new()?;
                    let _priority = Priority::new();
                    let _streaming = butterpollo_windows::timing::StreamingScope::enter();
                    let mut c = effective_config(&h, &s.launch)?;
                    if s.config.vrr_low_latency {
                        c.values.insert("wgc_slot_aligned_publish".into(), "false".into());
                    }
                    let timer = butterpollo_windows::timing::Timer::new()?;
                    let output = s
                        .launch
                        .client
                        .extra
                        .get("output_name_override")
                        .and_then(serde_json::Value::as_str)
                        .filter(|s| !s.is_empty())
                        .unwrap_or(c.get("output_name", ""));
                    if s.launch.role == Role::InputOnly {
                        let monitors = butterpollo_windows::display::monitors()?;
                        let monitor = monitors
                            .iter()
                            .find(|m| m.matches(output))
                            .or_else(|| monitors.iter().find(|m| m.primary))
                            .context("input display unavailable")?;
                        *s.output.write().unwrap() = monitor.display_name.clone();
                        let _client_commands = crate::process::ClientCommands::start(&h, &s)?;
                        while !s.stopping() && !h.stop.load(Ordering::Acquire) {
                            thread::sleep(Duration::from_millis(10));
                        }
                        return Ok(());
                    }
                    let initial = s
                        .launch
                        .preparation
                        .lock()
                        .unwrap()
                        .take()
                        .map(|p| p.downcast::<Arc<crate::display_session::Ready>>())
                        .transpose()
                        .map_err(|_| anyhow::anyhow!("invalid launch preparation"))?;
                    let prepared = match initial {
                        Some(p) if p.matches(&s.config) => *p,
                        previous => {
                            drop(previous);
                            h.app_display.lock().unwrap().remove(&s.launch.client.uuid);
                            crate::display_session::Ready::new(
                                crate::display_session::Prepared::create(
                                    &h, &s.launch, &s.config, &c,
                                )?,
                            )?
                        }
                    };
                    if s.launch.role == Role::Stream {
                        h.app_display
                            .lock()
                            .unwrap()
                            .insert(s.launch.client.uuid.clone(), (prepared.clone(), None));
                    }
                    let output = prepared.output();
                    *s.output.write().unwrap() = output.clone();
                    // Declaration order closes the encoder and joins capture before the display lease is removed.
                    let mut use_truehdr = s.config.hdr && rtx_enabled(&c);
                    let mut latest = m.capture(
                        &prepared.capture(),
                        s.config.hdr,
                        &c,
                        butterpollo_core::framegen::Rate(s.config.fps_millihz()),
                        &s.launch.id,
                        prepared.clone(),
                    )?;
                    let mut capture_wake = latest.subscribe()?;
                    let first = {
                        let deadline = Instant::now() + Duration::from_secs(10);
                        loop {
                            if let Some(image) = latest.current()? { break image; }
                            if s.stopping() || h.stop.load(Ordering::Acquire) {
                                return Ok(());
                            }
                            if Instant::now() >= deadline {
                                anyhow::bail!("capture produced no GPU frame");
                            }
                            if let Some(image) = latest.wait_for_frame(&timer, &capture_wake, (Instant::now() + Duration::from_millis(50)).min(deadline))? { break image; }
                        }
                    };
                    let mut encoder = Some(Encoder::new_gpu_options(
                        &s.config,
                        c.get("encoder", "auto"),
                        &first,
                        &c,
                    )?);
                    tracing::info!(width=s.config.width,height=s.config.height,fps=f64::from(s.config.fps_millihz())/1000.,codec=s.config.codec,hdr=s.config.hdr,vrr=s.config.vrr_low_latency,capture=%prepared.capture(),encoder=c.get("encoder","auto"),source_width=first.width,source_height=first.height,source_pixel=?first.pixel,"stream configured");
                    let metadata = first.gpu.hdr_metadata();
                    *s.hdr_metadata.write().unwrap() = metadata;
                    if let Some(encoder) = encoder.as_mut() {
                        encoder.set_hdr_metadata(metadata);
                    }
                    let mut truehdr = if use_truehdr {
                        truehdr_filter(&first, &c)
                    } else {
                        None
                    };
                    drop(first);
                    let mut truehdr_staging = None;
                    let _client_commands = crate::process::ClientCommands::start(&h, &s)?;
                    let audio_m = m.clone();
                    let audio_h = h.clone();
                    let audio_s = s.clone();
                    let audio = thread::Builder::new().name("audio".into()).spawn(move || {
                        if let Err(e) = audio_m.audio(audio_h, audio_s.clone()) {
                            tracing::warn!(error=%e,"audio worker stopped");
                        }
                    })?;
                    let mut packetizer = VideoPacketizer {
                        sequence: 0,
                        iv_counter: 0,
                        frame: 1,
                        packet_size: s.config.packet_size,
                        fec_percent: c.integer("fec_percentage", 20).clamp(0, 100) as usize,
                        min_fec: s.config.min_fec,
                        key: if s.config.encryption & 2 != 0 {
                            Some(s.launch.key)
                        } else {
                            None
                        },
                    };
                    let next_wire_frame = std::cell::Cell::new(u64::from(packetizer.frame));
                    let start = Instant::now();
                    let mut present_stamper = (s.config.codec != 3 && prepared.capture() == "wgc").then(butterpollo_windows::present_timing::Stamper::default);
                    let pyrowave_sender = if s.config.codec == 3 { Some(crate::pyrowave_send::Sender::new(m.video.clone(),s.clone(),c.clone(),h.clone(),start,prepared.capture() == "wgc")?) } else { None };
                    let period = butterpollo_core::framegen::Rate(s.config.fps_millihz()).period();
                    let mut cadence = butterpollo_core::stream_policy::Cadence::new(Instant::now(), period, c.boolean("wgc_pacing_smoothing", true));
                    let arrival_pacing = !s.config.vrr_low_latency
                        && butterpollo_core::stream_policy::Pacing::from_config(&c) == butterpollo_core::stream_policy::Pacing::Arrival;
                    let mut pacer = butterpollo_core::stream_policy::Pacer::new(Instant::now(), period);
                    let due = cadence.deadline();
                    let mut send_due = due;
                    let mut last_stamp = start;
                    let mut live_at = due;
                    let mut rebuild_encoder = false;
                    let mut runtime_config = c.clone();
                    let mut profiles = None;
                    let mut foreground = None;
                    let mut profile_due = Instant::now();
                    let mut metadata_due = Instant::now() + Duration::from_secs(1);
                    let mut timing_due = Instant::now() + Duration::from_secs(5);
                    let minimum_fps = c
                        .get("minimum_fps_target", if s.config.codec == 3 { "0" } else { "20" })
                        .parse::<f64>()
                        .unwrap_or(20.);
                    let minimum_fps = if minimum_fps > 0. {
                        minimum_fps.clamp(1., f64::from(s.config.fps_millihz()) / 1000.)
                    } else if s.config.codec == 3 {
                        f64::from(s.config.fps_millihz()) / 1000.
                    } else {
                        (f64::from(s.config.fps_millihz()) / 5000.).max(10.)
                    };
                    let static_period = Duration::from_secs_f64(1. / minimum_fps);
                    let limit_static_rate = minimum_fps < f64::from(s.config.fps_millihz()) / 1000.;
                    let mut last_image: Option<Arc<GpuImage>> = None;
                    let mut encoded_at = Instant::now();
                    // Where a new frame's age comes from: Windows to the capture
                    // worker, then the capture worker to the encoder's claim.
                    let mut claim_ages: Vec<(u64, u64)> = Vec::with_capacity(1024);
                    // The first pacing decision for the newest fresh frame, kept
                    // for the per-claim trace.
                    let mut first_seen: Option<(usize, Instant, Option<Duration>, Option<Instant>)> = None;
                    let mut batch = butterpollo_windows::net::Batch::default();
                    let batch_kb = match c.integer("video_max_batch_size_kb", 64) {
                        16 => 16,
                        32 => 32,
                        _ => 64,
                    };
                    let mut send_frames = |output: Vec<butterpollo_windows::encoder::Encoded>,
                                           peer: std::net::SocketAddr,
                                           call_latency: Duration|
                     -> Result<()> {
                        if let Some(sender) = &pyrowave_sender { return sender.submit(output,peer,call_latency); }
                        let polled = Instant::now();
                        let micros = |d: Duration| d.as_micros().min(u128::from(u64::MAX)) as u64;
                        for frame in output {
                            let encode = frame.latency.unwrap_or(call_latency);
                            let latency = micros(encode);
                            s.stats.latency_us.store(latency, Ordering::Relaxed);
                            // Moonlight's host latency runs from the claim to the
                            // packet, as the previous host measured it. Waiting
                            // before the claim is recorded as frame age.
                            let claimed = polled.checked_sub(encode).unwrap_or(polled);
                            let captured = frame.presentation.unwrap_or(claimed);
                            let age = micros(claimed.saturating_duration_since(captured));
                            let processing = micros(Instant::now().saturating_duration_since(claimed));
                            let stamp = present_stamper.as_mut().map_or(captured, |stamper| stamper.stamp(captured, &prepared.output())).max(last_stamp + Duration::from_nanos(11_112));
                            last_stamp = stamp;
                            let timestamp = (stamp.saturating_duration_since(start).as_secs_f64() * 90000.) as u32;
                            let packets = packetizer.encode_recovery(&frame.bytes,frame.idr,frame.after_invalidation,timestamp,processing)?;
                            next_wire_frame.set(u64::from(packetizer.frame));
                            let frame_bytes = packets.iter().map(|p|p.len() as u64).sum();
                            let bps = c.integer("pacing_max_bitrate_kbps", 0);
                            let bps = if bps > 0 {
                                (bps as u64 * 1000)
                                    .max(u64::from(s.bitrate.load(Ordering::Relaxed)) * 1100)
                            } else {
                                800_000_000
                            };
                            send_due = send_due.max(Instant::now());
                            let mut remaining = packets.as_slice();
                            while !remaining.is_empty() {
                                let now = Instant::now();
                                if send_due > now {
                                    timer.until_precise(send_due);
                                }
                                let budget = (bps / 4000)
                                    .clamp(remaining[0].len() as u64, batch_kb * 1024)
                                    as usize;
                                let count =
                                    butterpollo_windows::net::Batch::count(remaining, budget);
                                let bytes = batch.send(&m.video, &remaining[..count], peer)?;
                                remaining = &remaining[count..];
                                send_due += Duration::from_secs_f64(bytes as f64 * 8. / bps as f64);
                                s.stats.packets.fetch_add(count as u64, Ordering::Relaxed);
                                s.stats.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
                            }
                            s.stats.frames.fetch_add(1, Ordering::Relaxed);
                            s.stats.performance.lock().unwrap().record_timing(Instant::now(),butterpollo_core::performance::Timing{encode:latency,host:processing,age},frame_bytes);
                        }
                        Ok(())
                    };
                    let result = (|| -> Result<()> {
                        while !s.stopping() && !h.stop.load(Ordering::Acquire) {
                            if Instant::now() >= live_at {
                                live_at = Instant::now() + Duration::from_millis(250);
                                runtime_config = effective_config(&h, &s.launch)?;
                                if s.config.vrr_low_latency {
                                    runtime_config.values.insert("wgc_slot_aligned_publish".into(), "false".into());
                                }
                                *s.output.write().unwrap() = prepared.output();
                                let runtime = &runtime_config;
                                let enabled = s.config.hdr && rtx_enabled(runtime);
                                if enabled != use_truehdr {
                                    truehdr = None;
                                    latest = m.capture(
                                        &prepared.capture(),
                                        s.config.hdr,
                                        runtime,
                                        butterpollo_core::framegen::Rate(s.config.fps_millihz()),
                                        &s.launch.id,
                                        prepared.clone(),
                                    )?;
                                    capture_wake = latest.subscribe()?;
                                    use_truehdr = enabled;
                                    rebuild_encoder = true;
                                }
                                if let Some(filter) = truehdr.as_mut() {
                                    filter.set_parameters(rtx_parameters(runtime));
                                }
                                if Instant::now() >= timing_due {
                                    timing_due = Instant::now() + Duration::from_secs(5);
                                    let timing = s.stats.performance.lock().unwrap().snapshot(Instant::now());
                                    let ms = |key: &str| timing[key].as_f64().unwrap_or(0.);
                                    let split = |pick: fn(&(u64, u64)) -> u64| {
                                        let mut values: Vec<u64> = claim_ages.iter().map(pick).collect();
                                        values.sort_unstable();
                                        let mean = values.iter().sum::<u64>() as f64 / values.len().max(1) as f64 / 1000.;
                                        let p95 = values.get(values.len().saturating_sub(1) * 95 / 100).copied().unwrap_or(0) as f64 / 1000.;
                                        (mean, p95)
                                    };
                                    let (detect_mean_ms, detect_p95_ms) = split(|age| age.0);
                                    let (claim_wait_mean_ms, claim_wait_p95_ms) = split(|age| age.1);
                                    claim_ages.clear();

                                    tracing::info!(
                                        fps=ms("fps"),
                                        host_mean_ms=ms("host_processing_mean_ms"),
                                        host_p95_ms=ms("host_processing_p95_ms"),
                                        host_p99_ms=ms("host_processing_p99_ms"),
                                        host_max_ms=ms("host_processing_max_ms"),
                                        encode_mean_ms=ms("encode_mean_ms"),
                                        encode_p95_ms=ms("encode_p95_ms"),
                                        encode_p99_ms=ms("encode_p99_ms"),
                                        frame_age_mean_ms=ms("frame_age_mean_ms"),
                                        frame_age_p95_ms=ms("frame_age_p95_ms"),
                                        present_to_send_mean_ms=ms("present_to_send_mean_ms"),
                                        present_to_send_p99_ms=ms("present_to_send_p99_ms"),
                                        detect_mean_ms,
                                        detect_p95_ms,
                                        claim_wait_mean_ms,
                                        claim_wait_p95_ms,

                                        send_interval_p95_ms=timing["send_interval_p95_ms"].as_f64().unwrap_or(0.),
                                        send_interval_p99_ms=timing["send_interval_p99_ms"].as_f64().unwrap_or(0.),
                                        send_interval_max_ms=timing["send_interval_max_ms"].as_f64().unwrap_or(0.),
                                        "stream timings"
                                    );
                                }
                            }
                            latest.check()?;
                            let peer = m
                                .peers
                                .lock()
                                .unwrap()
                                .get(&(s.launch.id.clone(), false))
                                .copied();
                            let Some(peer) = peer else {
                                if start.elapsed() > crate::network::ping_timeout(&c) {
                                    anyhow::bail!("client video ping timed out");
                                }
                                timer.until(Instant::now() + Duration::from_millis(1));
                                continue;
                            };
                            let now = Instant::now();
                            let due = cadence.deadline();
                            if !s.config.vrr_low_latency && !arrival_pacing && now < due {
                                while Instant::now() < due {
                                    if encoder.as_ref().is_some_and(Encoder::pending) {
                                        send_frames(encoder.as_mut().map_or(Ok(vec![]), Encoder::poll)?, peer, Duration::ZERO)?;
                                        if encoder.as_ref().is_some_and(Encoder::pending) {
                                            timer.until(
                                                (Instant::now() + OUTPUT_POLL)
                                                    .min(due),
                                            );
                                        }
                                    } else {
                                        timer.until(due);
                                    }
                                }
                            }
                            let image = latest.wait_for_frame(&timer, &capture_wake, Instant::now() + period.min(Duration::from_millis(50)))?;
                            let Some(image) = image else {
                                // The capture is being re-created on a new device.
                                // Release everything on the old one so Windows can
                                // give the new capture a current adapter.
                                if encoder.take().is_some() {
                                    tracing::info!(client = %s.launch.client.name, "releasing the encoder while the capture recovers");
                                }
                                last_image = None;
                                truehdr = None;
                                truehdr_staging = None;
                                continue;
                            };
                            // Resolution changes or DXGI loss can recreate the capture device.
                            rebuild_encoder |= encoder
                                .as_ref()
                                .is_none_or(|encoder| !encoder.accepts_gpu_device(&image));
                            let fresh = last_image.as_ref().is_none_or(|previous| !Arc::ptr_eq(previous, &image));
                            if arrival_pacing
                                && fresh
                                && !rebuild_encoder
                                && !s.idr.load(Ordering::Acquire)
                                && s.invalidation.lock().unwrap().is_none()
                                && let interval = latest.source_interval()
                                && let butterpollo_core::stream_policy::Pace::WaitUntil(deadline) =
                                    pacer.decide(Instant::now(), image.captured, interval)
                            {
                                let key = Arc::as_ptr(&image) as usize;
                                if first_seen.is_none_or(|(seen, ..)| seen != key) {
                                    first_seen = Some((key, Instant::now(), interval, Some(deadline)));
                                }
                                if encoder.as_ref().is_some_and(Encoder::pending) {
                                    send_frames(encoder.as_mut().map_or(Ok(vec![]), Encoder::poll)?, peer, Duration::ZERO)?;
                                }
                                let until = if encoder.as_ref().is_some_and(Encoder::pending) {
                                    deadline.min(Instant::now() + OUTPUT_POLL)
                                } else {
                                    deadline
                                };
                                latest.wait_if_current(&timer, &capture_wake, &image, until)?;
                                continue;
                            }
                            if !rebuild_encoder
                                && (s.config.vrr_low_latency || limit_static_rate || arrival_pacing)
                                && !s.idr.load(Ordering::Acquire)
                                && s.invalidation.lock().unwrap().is_none()
                                && last_image
                                    .as_ref()
                                    .is_some_and(|previous| Arc::ptr_eq(previous, &image))
                                && encoded_at.elapsed() < static_period
                            {
                                if encoder.as_ref().is_some_and(Encoder::pending) { send_frames(encoder.as_mut().map_or(Ok(vec![]), Encoder::poll)?, peer, Duration::ZERO)?; }
                                let wait = if encoder.as_ref().is_some_and(Encoder::pending) { OUTPUT_POLL } else { period };
                                latest.wait_if_current(&timer, &capture_wake, &image, Instant::now() + wait.min(static_period.saturating_sub(encoded_at.elapsed())))?;
                                continue;
                            }
                            if use_truehdr && Instant::now() >= profile_due {
                                profile_due = Instant::now() + Duration::from_millis(250);
                                let (active, owned) = {
                                    let app = h.current_app.lock().unwrap();
                                    (app.is_some(), app.as_ref().and_then(|app| app.child.as_ref()).and_then(|child| child.process_ids().ok()).unwrap_or_default())
                                };
                                let visible = if active { foreground.get_or_insert_with(butterpollo_windows::foreground::Tracker::default).poll(&owned, &image.gpu.display) } else { None };
                                let profiles = profiles.get_or_insert_with(butterpollo_windows::rtx_profiles::Profiles::new);
                                runtime_config = butterpollo_core::rtx_policy::resolve(&runtime_config, visible.is_some(), profiles.poll(visible.as_deref()));
                                if let Some(filter) = truehdr.as_mut() { filter.set_parameters(rtx_parameters(&runtime_config)); }
                            }
                            let rebuilt = rebuild_encoder;
                            if rebuild_encoder {
                                encoder = None;
                                encoder = Some(Encoder::new_gpu_options(
                                    &s.config,
                                    c.get("encoder", "auto"),
                                    &image,
                                    &c,
                                )?);
                                metadata_due = Instant::now();
                                truehdr = if use_truehdr {
                                    truehdr_filter(&image, &runtime_config)
                                } else {
                                    None
                                };
                                truehdr_staging = None;
                                s.request_idr();
                                rebuild_encoder = false;
                            }
                            let active = encoder
                                .as_mut()
                                .expect("a missing encoder is rebuilt above");
                            if rebuilt {
                                active.set_next_frame(next_wire_frame.get());
                            }
                            let begin = Instant::now();
                            if fresh && tracing::enabled!(target: "pacing", tracing::Level::TRACE) {
                                let us = |at: Instant| at.saturating_duration_since(start).as_micros() as u64;
                                let key = Arc::as_ptr(&image) as usize;
                                let (seen, interval, deadline) = match first_seen {
                                    Some((k, seen, interval, deadline)) if k == key => (seen, interval, deadline),
                                    _ => (begin, latest.source_interval(), None),
                                };
                                tracing::trace!(
                                    target: "pacing",
                                    presented = us(image.captured),
                                    acquired = us(image.acquired),
                                    seen = us(seen),
                                    claim = us(begin),
                                    interval = interval.map_or(0, |i| i.as_micros() as u64),
                                    deadline = deadline.map_or(0, us),
                                    "claim"
                                );
                            }
                            if fresh && claim_ages.len() < 4096 {
                                let micros = |d: Duration| d.as_micros().min(u128::from(u64::MAX)) as u64;
                                claim_ages.push((
                                    micros(image.acquired.saturating_duration_since(image.captured)),
                                    micros(begin.saturating_duration_since(image.acquired)),
                                ));
                            }
                            if Instant::now() >= metadata_due {
                                let metadata = image.gpu.hdr_metadata();
                                *s.hdr_metadata.write().unwrap() = metadata;
                                active.set_hdr_metadata(metadata);
                                metadata_due = Instant::now() + Duration::from_secs(1);
                            }
                            if let Some((first, last)) = s.invalidation.lock().unwrap().take()
                                && !active.invalidate_ref_frames(first, last)
                            {
                                s.request_idr();
                            }
                            let idr = s.idr.swap(false, Ordering::AcqRel);
                            let bitrate = s.bitrate.load(Ordering::Acquire);
                            let converted = truehdr.is_some()
                                && image.pixel == butterpollo_windows::capture::Pixel::Bgra8;
                            let scale = if converted {
                                (runtime_config
                                    .integer("rtx_hdr_peak_brightness", 1000)
                                    .clamp(400, 2000) as f32
                                    / 1000.)
                                    .max(1.)
                            } else {
                                1.
                            };
                            active.set_luminance(
                                100. + runtime_config
                                    .integer("rtx_hdr_sdr_brightness", 0)
                                    .clamp(0, 100) as f32,
                                scale,
                            );
                            let mut presented_image = image.as_ref().clone();
                            if last_image.as_ref().is_some_and(|previous| Arc::ptr_eq(previous,&image)) { presented_image.captured = Instant::now(); }
                            let transformed = if converted { truehdr.as_mut().map(|filter| filter.apply_gpu(&presented_image)).transpose() } else { Ok(None) };
                            let output = if let Ok(Some(transformed)) = transformed.as_ref() {
                                active.encode_gpu(transformed, idr, bitrate)?
                            } else if let Err(error) = transformed {
                                tracing::warn!(%error, "TrueHDR conversion failed; continuing with SDR-to-PQ");
                                truehdr = None;
                                active.set_luminance(100. + runtime_config.integer("rtx_hdr_sdr_brightness",0).clamp(0,100) as f32, 1.);
                                active.encode_gpu(&presented_image, idr, bitrate)?
                            } else if c.boolean("wgc_direct_encoder_input", true) {
                                active.encode_gpu(&presented_image, idr, bitrate)?
                            } else {
                                active.encode(
                                    &presented_image.readback(&mut truehdr_staging)?,
                                    idr,
                                    bitrate,
                                )?
                            };
                            let call_latency = begin.elapsed();
                            // Repeat deadlines start at submission, so encoder work
                            // does not extend the interval between static frames.
                            encoded_at = begin;
                            if arrival_pacing {
                                pacer.claimed(begin);
                                latest.grid.lock().unwrap().anchor = pacer.allowed_at(begin);
                            } else {
                                cadence.submitted(begin);
                                latest.grid.lock().unwrap().anchor = cadence.deadline();
                            }
                            last_image = Some(image);
                            send_frames(output, peer, call_latency)?;
                        }
                        Ok(())
                    })();
                    s.stop();
                    let _ = audio.join();
                    result
                })();
                if let Err(e) = result {
                    tracing::error!(error=%e,client=%s.launch.client.name,"session failed");
                }
                s.stop();
                m.peers
                    .lock()
                    .unwrap()
                    .retain(|(id, _), _| id != &s.launch.id);
                h.sessions.lock().unwrap().active.remove(&s.launch.id);
                if s.launch.role == Role::RemoteMonitor
                    && h.config
                        .read()
                        .unwrap()
                        .boolean("remote_monitor_disconnect_on_stream_end", false)
                {
                    crate::remote_display::disconnect(&h, Some(&s.launch.client.uuid));
                }
                tracing::info!(client=%s.launch.client.name,"CLIENT DISCONNECTED");
            });
        if let Err(e) = result {
            tracing::error!(error=%e,"could not start session worker");
        }
    }
    fn audio(&self, h: Shared, s: Arc<Session>) -> Result<()> {
        let _com = ComGuard::new()?;
        let _priority = Priority::new();
        let config = effective_config(&h, &s.launch)?;
        let muted = !config.boolean("stream_audio", true)
            || (s.launch.role == Role::RemoteMonitor
                && config.boolean("remote_monitor_mute_audio", false));
        let mut route = s
            .launch
            .audio_preparation
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|route| route.downcast_ref::<Arc<butterpollo_windows::audio_route::Route>>())
            .cloned();
        let mut capture: Option<Loopback> = None;
        let mut sink = String::new();
        let mut audio_check = Instant::now();
        let mut capture_failed = false;
        let mut audio_error: Option<String> = None;
        let directory = std::env::current_exe()?.parent().unwrap().to_owned();
        let layout = butterpollo_core::audio::OpusLayout::select(
            s.config.audio_channels as usize,
            s.config.audio_quality,
            s.launch.options.get("surroundParams").map(String::as_str),
        )?;
        let mut opus = Opus::new_layout_duration(&directory, &layout, s.config.audio_packet_ms)?;
        let mut p = AudioPacketizer::new(
            s.launch.key,
            s.launch.key_id,
            s.config.encryption & 4 != 0,
            u32::from(s.config.audio_packet_ms),
        );
        let frames = 48 * usize::from(s.config.audio_packet_ms);
        let start = Instant::now();
        let mut next = Instant::now();
        let interval = Duration::from_millis(u64::from(s.config.audio_packet_ms));
        let timer = butterpollo_windows::timing::Timer::new()?;
        let silence = vec![0.; frames * s.config.audio_channels as usize];
        while !s.stopping() && !h.stop.load(Ordering::Acquire) {
            timer.until(next);
            if !muted && Instant::now() >= audio_check {
                audio_check = Instant::now() + Duration::from_secs(1);
                if route.is_none() {
                    match butterpollo_windows::audio_route::Route::acquire(
                        &config,
                        &h.directory,
                        s.launch.host_audio,
                        s.config.audio_channels as usize,
                    ) {
                        Ok(value) => {
                            let value = Arc::new(value);
                            if s.launch.role == Role::Stream
                                && h.current_app.lock().unwrap().is_some()
                            {
                                *h.app_audio.lock().unwrap() = Some(value.clone());
                            }
                            route = Some(value);
                        }
                        Err(error) => {
                            let detail = format!("{error:#}");
                            if audio_error.as_ref() != Some(&detail) {
                                tracing::warn!(error=%detail, "audio routing failed; retrying");
                                audio_error = Some(detail);
                            }
                        }
                    }
                }
                if let Some(route) = &route {
                    if let Err(error) = route.maintain_default() {
                        tracing::debug!(%error, "audio default could not be maintained");
                    }
                    let selected = route
                        .capture_sink(&config)
                        .unwrap_or_else(|_| route.sink.clone());
                    if selected != sink {
                        capture = None;
                        sink = selected;
                        capture_failed = false;
                    }
                    if capture.is_none()
                        && (!capture_failed || config.boolean("auto_capture_sink", true))
                    {
                        if let Err(error) = route.set_channels(s.config.audio_channels as usize) {
                            tracing::warn!(%error, "virtual surround format could not be applied");
                        }
                        match Loopback::new_sink(s.config.audio_channels as usize, &sink) {
                            Ok(value) => {
                                capture = Some(value);
                                capture_failed = false;
                                audio_error = None;
                                tracing::info!(
                                    channels = s.config.audio_channels,
                                    "WASAPI audio capture started"
                                );
                            }
                            Err(error) => {
                                capture_failed = true;
                                let detail = format!("{error:#}");
                                if audio_error.as_ref() != Some(&detail) {
                                    tracing::warn!(error=%detail, "WASAPI audio capture failed; retrying");
                                    audio_error = Some(detail);
                                }
                            }
                        }
                    }
                }
            }
            let peer = self
                .peers
                .lock()
                .unwrap()
                .get(&(s.launch.id.clone(), true))
                .copied();
            let samples = match capture.as_mut().map(|capture| capture.read(frames)) {
                Some(Ok(value)) => value,
                Some(Err(error)) => {
                    tracing::warn!(%error, "audio endpoint changed; reopening WASAPI");
                    capture = None;
                    capture_failed = true;
                    None
                }
                None => None,
            };
            if let Some(peer) = peer {
                for packet in p.encode(&opus.encode(samples.as_deref().unwrap_or(&silence))?)? {
                    self.audio.send_to(&packet, peer)?;
                }
            } else if start.elapsed() > crate::network::ping_timeout(&config) {
                anyhow::bail!("client audio ping timed out");
            }
            next += interval;
            if next < Instant::now() {
                next = Instant::now() + interval;
            }
        }
        Ok(())
    }
    fn control(&self, h: Shared) -> Result<()> {
        let socket = crate::network::udp((self.bind, self.control_port).into())?;
        butterpollo_windows::net::configure_udp(&socket)?;
        let mut host = Host::new(
            socket,
            HostSettings {
                peer_limit: 32,
                channel_limit: 16,
                ..Default::default()
            },
        )?;
        let _com = ComGuard::new()?;
        let _priority = Priority::new();
        let mut peers: HashMap<PeerID, ControlPeer> = HashMap::new();
        let input_timer = butterpollo_windows::timing::Timer::new()?;
        let mut feedback_at = Instant::now();
        while !h.stop.load(Ordering::Acquire) {
            let ping_timeout = crate::network::ping_timeout(&h.config.read().unwrap());
            // Bound each pass so a busy input peer cannot starve cleanup or feedback.
            for _ in 0..512 {
                let Some(event) = host.service()? else { break };
                match event {
                    Event::Connect { peer, data } => {
                        let address = peer.address().context("ENet peer address missing")?;
                        let sessions = h.sessions.lock().unwrap();
                        let candidates: Vec<_> = sessions
                            .active
                            .values()
                            .map(|s| &s.launch)
                            .chain(sessions.pending.values())
                            .filter(|l| l.peer == address.ip().to_canonical())
                            .collect();
                        let launch = candidates
                            .iter()
                            .find(|l| data == l.connect_data)
                            .copied()
                            .or_else(|| {
                                if data == 0 && candidates.len() == 1 {
                                    Some(candidates[0])
                                } else {
                                    None
                                }
                            });
                        if let Some(l) = launch {
                            if peers.values().any(|p| p.id == l.id) {
                                peer.disconnect_now(0);
                                continue;
                            }
                            peers.insert(
                                peer.id(),
                                ControlPeer {
                                    id: l.id.clone(),
                                    injector: None,
                                    sequence: 0,
                                    received: Default::default(),
                                    hdr_metadata: None,
                                    legacy: butterpollo_core::packet::LegacyInput::new(l.key_id),
                                    seen: Instant::now(),
                                    session: None,
                                    inputs: vec![],
                                    command_at: None,
                                },
                            );
                        } else {
                            peer.disconnect_now(0);
                        }
                    }
                    Event::Disconnect { peer, .. } => {
                        if let Some(p) = peers.remove(&peer.id()) {
                            let session = h.sessions.lock().unwrap().active.get(&p.id).cloned();
                            h.sessions.lock().unwrap().request_stop(Some(&p.id));
                            if let Some(session) = session
                                && session.launch.role == Role::RemoteMonitor
                                && h.config.read().unwrap().boolean(
                                    "remote_monitor_disconnect_on_client_disconnect",
                                    false,
                                )
                            {
                                crate::remote_display::disconnect(
                                    &h,
                                    Some(&session.launch.client.uuid),
                                );
                            }
                        }
                    }
                    Event::Receive { peer, packet, .. } => {
                        let Some(p) = peers.get_mut(&peer.id()) else {
                            continue;
                        };
                        let s = h.sessions.lock().unwrap().active.get(&p.id).cloned();
                        let Some(s) = s else { continue };
                        p.session = Some(s.clone());
                        let b = packet.data();
                        if b.len() < 2 || b.len() > 65536 {
                            continue;
                        }
                        let encrypted = b[..2] == [1, 0];
                        if s.config.encryption & 1 != 0 && !encrypted {
                            continue;
                        }
                        let parsed = if encrypted {
                            butterpollo_core::packet::decrypt_control(
                                &s.launch.key,
                                b,
                                s.config.encryption & 1 != 0,
                            )
                            .and_then(|(seq, kind, payload)| {
                                if !p.received.accept(seq) {
                                    anyhow::bail!("replayed control message");
                                }
                                Ok((kind, payload))
                            })
                        } else {
                            butterpollo_core::packet::control_header(b)
                                .map(|(kind, payload)| (kind, payload.to_vec()))
                        };
                        let (kind, payload) = match parsed {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::debug!(error=%e,"invalid control packet");
                                continue;
                            }
                        };
                        p.seen = Instant::now();
                        match kind {
                            0x3000 if s.launch.client.allows(1 << 20) && payload.len() == 1 => {
                                if h.current_app
                                    .lock()
                                    .unwrap()
                                    .as_ref()
                                    .is_some_and(|app| !app.allow_client_commands)
                                {
                                    continue;
                                }
                                if p.command_at
                                    .is_some_and(|last| last.elapsed() < Duration::from_secs(1))
                                {
                                    continue;
                                }
                                p.command_at = Some(Instant::now());
                                let commands: serde_json::Value = serde_json::from_str(
                                    h.config.read().unwrap().get("server_cmd", "[]"),
                                )
                                .unwrap_or_default();
                                if let Some(command) =
                                    commands.as_array().and_then(|a| a.get(payload[0] as usize))
                                    && let Some(value) =
                                        command.get("cmd").and_then(serde_json::Value::as_str)
                                {
                                    let elevated = command
                                        .get("elevated")
                                        .and_then(serde_json::Value::as_bool)
                                        .unwrap_or(false);
                                    if let Err(e) =
                                        butterpollo_windows::process::Process::shell_detached(
                                            value,
                                            None,
                                            elevated,
                                            &Default::default(),
                                        )
                                    {
                                        tracing::warn!(error=%e, "configured server command failed");
                                    }
                                }
                            }
                            0x0301 if payload.len() == 8 => {
                                s.request_invalidation(
                                    u64::from(u32::from_le_bytes(payload[..4].try_into().unwrap())),
                                    u64::from(u32::from_le_bytes(
                                        payload[4..8].try_into().unwrap(),
                                    )),
                                );
                            }
                            0x0301 | 0x0302 => s.request_idr(),
                            0x0109 => {
                                if encrypted {
                                    s.stop();
                                }
                            }
                            0x0206 => {
                                let raw = if encrypted {
                                    Ok(payload)
                                } else {
                                    p.legacy.open(&s.launch.key, &payload)
                                };
                                let event = raw.and_then(|raw| input::decode(&raw));
                                match event {
                                    Ok(event)
                                        if s.launch.client.allows(event.required_permission()) =>
                                    {
                                        if p.inputs.last_mut().is_some_and(|last| {
                                            last.merge(&event) == input::Batch::Merged
                                        }) {
                                            continue;
                                        }
                                        if p.inputs.len() < 512 {
                                            p.inputs.push(event);
                                        } else {
                                            s.stop();
                                        }
                                    }
                                    Err(e) => tracing::debug!(error=%e,"invalid input packet"),
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            let poll_feedback = Instant::now() >= feedback_at;
            if poll_feedback {
                feedback_at = Instant::now() + Duration::from_millis(8);
            }
            let mut remove = vec![];
            for (peer_id, p) in &mut peers {
                let sessions = h.sessions.lock().unwrap();
                let s = sessions
                    .active
                    .get(&p.id)
                    .cloned()
                    .or_else(|| p.session.clone());
                let pending = sessions.pending.contains_key(&p.id);
                drop(sessions);
                if let Some(s) = s {
                    p.session = Some(s.clone());
                    if s.stopping() || p.seen.elapsed() > ping_timeout {
                        if let Ok(message) = p.encrypt(&s, 0x0109, &0x80030023u32.to_be_bytes()) {
                            let peer = host.peer_mut(*peer_id);
                            let _ = peer.send(0, &Packet::new(message, PacketKind::Reliable));
                            peer.disconnect_later(0);
                        } else {
                            host.peer_mut(*peer_id).disconnect_now(0);
                        }
                        s.stop();
                        remove.push(*peer_id);
                        continue;
                    }
                    let metadata = s.hdr_metadata.read().unwrap().wire(s.config.hdr);
                    if p.hdr_metadata != Some(metadata)
                        && !s.output.read().unwrap().is_empty()
                        && let Ok(message) = p.encrypt(&s, 0x010e, &metadata)
                        && host
                            .peer_mut(*peer_id)
                            .send(0, &Packet::new(message, PacketKind::Reliable))
                            .is_ok()
                    {
                        p.hdr_metadata = Some(metadata);
                    }
                    if !p.inputs.is_empty() && p.injector.is_none() {
                        let c = effective_config(&h, &s.launch)?;
                        let output = s.output.read().unwrap().clone();
                        match Injector::new_options(
                            if output.is_empty() {
                                c.get("output_name", "")
                            } else {
                                &output
                            },
                            c.get("gamepad", "auto"),
                            &c,
                        ) {
                            Ok(i) => p.injector = Some(i),
                            Err(e) => tracing::warn!(error=%e,"input initialization failed"),
                        }
                    }
                    for event in std::mem::take(&mut p.inputs) {
                        if let Some(i) = &mut p.injector {
                            match i.apply(&event) {
                                Err(e) => tracing::debug!(error=%e,"input injection failed"),
                                Ok(()) => {
                                    if let input::Input::Arrival {
                                        id, capabilities, ..
                                    } = event
                                        && i.gamepads
                                            .as_ref()
                                            .is_some_and(|g| g.motion_supported(u16::from(id)))
                                    {
                                        for (cap, kind) in [(0x10, 1), (0x20, 2)] {
                                            if capabilities & cap != 0 {
                                                let mut payload =
                                                    u16::from(id).to_le_bytes().to_vec();
                                                payload.extend_from_slice(&200u16.to_le_bytes());
                                                payload.push(kind);
                                                if let Ok(message) = p.encrypt(&s, 0x5501, &payload)
                                                {
                                                    let _ = host.peer_mut(*peer_id).send(
                                                        1,
                                                        &Packet::new(message, PacketKind::Reliable),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if poll_feedback {
                        let mut messages = Vec::new();
                        if let Some(i) = &mut p.injector
                            && let Err(e) = i.refresh()
                        {
                            tracing::debug!(error=%e,"pointer refresh failed");
                        }
                        if let Some(i) = &mut p.injector
                            && let Some(g) = &mut i.gamepads
                        {
                            let feedback = g.feedback().unwrap_or_default();
                            for (id, kind, data) in feedback {
                                messages.extend(
                                    feedback_packets(id, kind, &data)
                                        .into_iter()
                                        .filter(|(kind, _)| i.feedback_allowed(*kind)),
                                );
                            }
                        }
                        for (kind, payload) in messages {
                            if let Ok(message) = p.encrypt(&s, kind, &payload) {
                                let _ = host
                                    .peer_mut(*peer_id)
                                    .send(1, &Packet::new(message, PacketKind::Reliable));
                            }
                        }
                    }
                } else if !pending || p.seen.elapsed() > ping_timeout {
                    host.peer_mut(*peer_id).disconnect_now(0);
                    remove.push(*peer_id);
                }
            }
            for peer in remove {
                peers.remove(&peer);
            }
            host.flush();
            // Sleep(1) can defer input until the next coarse Windows tick.
            input_timer.until(Instant::now() + Duration::from_millis(1));
        }
        for (peer, p) in peers {
            host.peer_mut(peer).disconnect_now(0);
            drop(p);
        }
        Ok(())
    }
}
struct ControlPeer {
    id: String,
    injector: Option<Injector>,
    sequence: u32,
    received: butterpollo_core::packet::ReplayWindow,
    hdr_metadata: Option<[u8; 27]>,
    legacy: butterpollo_core::packet::LegacyInput,
    seen: Instant,
    session: Option<Arc<Session>>,
    inputs: Vec<input::Input>,
    command_at: Option<Instant>,
}
impl ControlPeer {
    fn encrypt(&mut self, s: &Session, kind: u16, payload: &[u8]) -> Result<Vec<u8>> {
        let seq = self.sequence;
        self.sequence = seq.checked_add(1).context("control nonce exhausted")?;
        butterpollo_core::packet::encrypted_control(
            &s.launch.key,
            seq,
            kind,
            payload,
            s.config.encryption & 1 != 0,
        )
    }
}
fn feedback_packets(id: u16, kind: u16, data: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let mut result = vec![];
    if matches!(kind, 1 | 3 | 4 | 5) && data.len() >= 8 {
        let mut rumble = 0x00c0ffeeu32.to_le_bytes().to_vec();
        rumble.extend_from_slice(&id.to_le_bytes());
        rumble.extend_from_slice(&data[..4]);
        result.push((0x010b, rumble));
        if kind == 4 {
            result.push((0x5500, [&id.to_le_bytes()[..], &data[4..8]].concat()));
        } else if kind == 1 || (kind == 5 && data[7] & 1 != 0) {
            result.push((0x5502, [&id.to_le_bytes()[..], &data[4..7]].concat()));
        }
        if kind == 5 && data.len() == 32 && data[7] & 2 != 0 {
            let mut trigger = id.to_le_bytes().to_vec();
            trigger.push(3);
            trigger.extend_from_slice(&data[10..21]);
            trigger.extend_from_slice(&data[21..32]);
            result.push((0x5503, trigger));
        }
    }
    result
}
