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
/// How long an encoder that fails mid-stream is recreated before the session
/// gives up: a GPU busy with a game or a driver reset costs frames, not the stream.
const ENCODER_RECOVERY: Duration = Duration::from_secs(5);
/// Keep one queued picture while an encode is running. Larger queues did not
/// improve throughput on an overloaded AMF encoder, but increased frame age.
const ENCODER_BACKLOG: usize = 2;
fn encoder_progress(
    failing: &mut Option<Instant>,
    produced_frame: bool,
    now: Instant,
) -> Result<()> {
    if produced_frame {
        *failing = None;
    } else if failing.is_some_and(|since| now.duration_since(since) >= ENCODER_RECOVERY) {
        anyhow::bail!("the encoder did not produce a frame during recovery");
    }
    Ok(())
}
/// Output the encoder has finished. An encoder that fails to deliver it is
/// dropped, so the next frame recreates it and asks for a keyframe, as after
/// a failed submission; only failures lasting [`ENCODER_RECOVERY`] end the stream.
fn collect(
    encoder: &mut Option<Encoder>,
    failing: &mut Option<Instant>,
    warnings: &butterpollo_core::session::Warnings,
) -> Result<Vec<butterpollo_windows::encoder::Encoded>> {
    let Some(active) = encoder.as_mut() else {
        return Ok(vec![]);
    };
    match active.poll() {
        Ok(output) => {
            encoder_progress(failing, !output.is_empty(), Instant::now())?;
            Ok(output)
        }
        Err(error) => {
            let since = *failing.get_or_insert_with(Instant::now);
            if since.elapsed() >= ENCODER_RECOVERY {
                return Err(error.context("the encoder kept failing"));
            }
            warnings.set("encoder_recovery", format!("Encoder output failed ({error:#}); recreating the same encoder while the picture freezes. Lower game GPU load or update the graphics driver if this repeats."));
            *encoder = None;
            Ok(vec![])
        }
    }
}
/// Backoff only after reopening fails; resource release is acknowledged.
const RECOVERY_RETRY: Duration = Duration::from_millis(150);
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
    butterpollo_core::rtx_policy::enabled(config)
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
    Ok(config)
}
fn apply_overrides(
    config: &mut Config,
    overrides: &serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    let overrides: serde_json::Map<String, serde_json::Value> = overrides
        .iter()
        .filter(|(_, v)| v.as_str() != Some(""))
        .filter(|(k, _)| {
            let allowed = butterpollo_core::config::override_allowed(k);
            if !allowed {
                tracing::warn!(key = %k, "ignoring an override of a host-wide setting");
            }
            allowed
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    // One at a time: an override the host cannot use (an imported frame
    // limit of -1) is skipped, as in the C++ host, instead of failing every
    // stream of the device or app, or the running stream when it is edited.
    let mut overrides = overrides;
    overrides.retain(|key, value| {
        let single = serde_json::Map::from_iter([(key.clone(), value.clone())]);
        match config.update(&single) {
            Ok(()) => true,
            Err(error) => {
                static WARNED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
                let warning = format!("{key}={value}");
                let mut warned = WARNED.lock().unwrap();
                if !warned.contains(&warning) && warned.len() < 64 {
                    warned.push(warning);
                    tracing::warn!(key, %value, error = format!("{error:#}"), "override ignored");
                }
                false
            }
        }
    });
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
    input::{Injector, PadReport},
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
    warnings: Arc<butterpollo_core::session::Warnings>,
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
/// A session's QoS tag on a shared socket, moved along when the client's
/// address changes.
#[derive(Default)]
struct Tagged(Option<(SocketAddr, Option<butterpollo_windows::net::QosFlow>)>);
impl Tagged {
    fn follow(&mut self, socket: &UdpSocket, peer: SocketAddr, voice: bool) {
        if self.0.as_ref().is_some_and(|(tagged, _)| *tagged == peer) {
            return;
        }
        let class = if voice { "voice" } else { "video" };
        let flow = match butterpollo_windows::net::QosFlow::new(socket, peer, voice) {
            Ok(Some(flow)) => {
                tracing::info!(%peer, class, "stream traffic tagged for QoS");
                Some(flow)
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%peer, class, error = %format!("{error:#}"), "could not tag stream traffic for QoS");
                None
            }
        };
        self.0 = Some((peer, flow));
    }
}
pub struct Media {
    video: Arc<UdpSocket>,
    audio: Arc<UdpSocket>,
    peers: Mutex<HashMap<(String, bool), SocketAddr>>,
    captures: Mutex<HashMap<CaptureKey, Weak<Source>>>,
    control_port: u16,
    bind: IpAddr,
}
#[derive(Debug, PartialEq, Eq, Hash)]
struct CaptureKey {
    kind: String,
    output: String,
    hdr: bool,
    adapter: String,
    adapter_id: String,
    phase: String,
    compute: bool,
    wgc_compute: bool,
    wgc_user_helper: bool,
    wgc_high_rate: bool,
    wgc_drain: bool,
    wgc_helper_scheduling: bool,
}
impl CaptureKey {
    fn new(kind: &str, output: &str, hdr: bool, config: &Config, phase: &str) -> Self {
        Self {
            kind: kind.into(),
            output: output.into(),
            hdr,
            adapter: config.get("adapter_name", "").into(),
            adapter_id: config.get("adapter_pnp_id", "").into(),
            phase: if config.boolean("wgc_slot_aligned_publish", false) {
                phase
            } else {
                ""
            }
            .into(),
            compute: config.boolean("gpu_compute_conversion", true),
            wgc_compute: config.boolean("wgc_compute_copy", true),
            wgc_user_helper: config.boolean("wgc_user_helper", false),
            wgc_high_rate: !matches!(kind, "ddx" | "dxgi")
                && config.boolean("wgc_high_rate_capture", false),
            wgc_drain: !matches!(kind, "ddx" | "dxgi")
                && config.boolean("wgc_drain_to_newest", false),
            wgc_helper_scheduling: !matches!(kind, "ddx" | "dxgi")
                && config.boolean("wgc_helper_streaming_scope", false),
        }
    }
}
/// ENet's UDP adapter ends the control worker on any error but WouldBlock,
/// and nothing restarted it: a Wi-Fi roam (WSAENETUNREACH), an ICMP reply
/// (WSAENETRESET) or an oversized datagram (WSAEMSGSIZE) left every later
/// session without input or control. These lose one datagram; ENet resends
/// what was reliable.
struct ControlSocket {
    socket: UdpSocket,
    /// Whether datagrams are held back, and those held, in order: ENet
    /// sends a packet's acknowledgement before it returns the packet, and
    /// that sendto delayed every input by its cost.
    holding: bool,
    held: Vec<(SocketAddr, Vec<u8>)>,
}
impl ControlSocket {
    fn new(socket: UdpSocket) -> Self {
        Self {
            socket,
            holding: false,
            held: Vec::new(),
        }
    }
    /// Hold back what ENet sends until release().
    fn hold(&mut self) {
        self.holding = true;
    }
    /// Send what was held, in order, and stop holding. A datagram that is
    /// lost is logged; any other error ends the worker, as it would have
    /// inside service().
    fn release(&mut self) -> std::io::Result<()> {
        self.holding = false;
        for (address, buffer) in self.held.drain(..) {
            if let Err(error) = rusty_enet::Socket::send(&mut self.socket, address, &buffer) {
                if !butterpollo_windows::net::datagram_lost(&error) {
                    return Err(error);
                }
                tracing::debug!(%error, %address, "control datagram dropped");
            }
        }
        Ok(())
    }
}
impl rusty_enet::Socket for ControlSocket {
    type Address = SocketAddr;
    type Error = std::io::Error;
    fn init(&mut self, options: rusty_enet::SocketOptions) -> std::io::Result<()> {
        rusty_enet::Socket::init(&mut self.socket, options)
    }
    fn send(&mut self, address: SocketAddr, buffer: &[u8]) -> std::io::Result<usize> {
        if self.holding {
            self.held.push((address, buffer.to_vec()));
            return Ok(buffer.len());
        }
        match rusty_enet::Socket::send(&mut self.socket, address, buffer) {
            Err(error) if butterpollo_windows::net::datagram_lost(&error) => Ok(0),
            result => result,
        }
    }
    fn receive(
        &mut self,
        buffer: &mut [u8; rusty_enet::MTU_MAX],
    ) -> std::io::Result<Option<(SocketAddr, rusty_enet::PacketReceived)>> {
        match rusty_enet::Socket::receive(&mut self.socket, buffer) {
            Err(error) if butterpollo_windows::net::datagram_lost(&error) => Ok(None),
            result => result,
        }
    }
}
fn capture_config(config: &Config) -> Config {
    let mut capture = config.clone();
    // Keep the effective interval choice with the shared capture and its helper.
    // A 1 ms request skipped updates in the 120 FPS motion comparison; use
    // explicit zero at every rate unless the user opts into the legacy limit.
    capture.values.insert(
        "wgc_high_rate_capture".into(),
        config.boolean("wgc_high_rate_capture", false).to_string(),
    );
    capture
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
                // Input and control for every session pass through this
                // worker: restart it rather than leave later sessions without.
                while !h.stop.load(Ordering::Acquire) {
                    match control.control(h.clone()) {
                        Ok(()) => break,
                        Err(e) => {
                            tracing::error!(error = %format!("{e:#}"), "control worker stopped; restarting");
                            thread::sleep(Duration::from_secs(1));
                        }
                    }
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
        let capture_config = capture_config(config);
        let key = CaptureKey::new(kind, &output, hdr, &capture_config, phase);
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
        let warnings = Arc::new(butterpollo_core::session::Warnings::default());
        let capture_warnings = warnings.clone();
        let grid = Arc::new(Mutex::new(butterpollo_windows::capture::ClaimGrid {
            anchor: Instant::now(),
            period: rate.period(),
        }));
        let worker_grid = grid.clone();
        let kind = kind.to_owned();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                let result = (|| -> Result<()> {
                    let _com = ComGuard::new()?;
                    let _priority = Priority::new();
                    let _display_awake = butterpollo_windows::timing::DisplayAwake::enter()
                        .inspect_err(|error| tracing::warn!(%error, "capture cannot keep the display awake"))
                        .ok();
                    let timer = butterpollo_windows::timing::Timer::new()?;
                    let mut target = prepared.capture_target();
                    // Duplicate the desktop that is showing, including the secure
                    // desktop of a UAC prompt or the lock screen.
                    butterpollo_windows::input::follow_input_desktop();
                    let mut capture =
                        match Capture::open_for_stream_reported(&target.0, &kind, hdr, &capture_config, capture_warnings.clone()) {
                            Ok(c) => c,
                            Err(e) => {
                                let _ = started_tx.send(Err(format!("{e:#}")));
                                return Ok(());
                            }
                        };
                    capture.set_claim_grid(worker_grid.clone(), aligned);
                    tracing::info!(requested=%kind, backend=capture.backend(), output=%target.0, "capture backend opened");
                    // Re-create the capture on a new device. Windows keeps handing
                    // out the stale adapter while anything holds the old device, so
                    // every duplication made then loses access at once (a display
                    // arriving for another client does this). The old capture is
                    // dropped and consumers acknowledge releasing their frames
                    // and encoders before the new device is made.
                    let reopen = |lost: Capture, target: &(String, u64)| -> Result<Capture> {
                        let recovery_started = Instant::now();
                        // Desktop Duplication reports the pointer's shape only
                        // when it changes; the new capture starts from this one.
                        let mut pointer = lost.pointer();
                        drop(lost);
                        worker.begin_recovery()?;
                        let deadline = Instant::now() + Duration::from_secs(30);
                        while !worker.consumers_released() {
                            if worker_stop.load(Ordering::Acquire) {
                                anyhow::bail!("capture stopped while releasing resources");
                            }
                            anyhow::ensure!(Instant::now() < deadline, "streams did not release the lost capture device");
                            timer.until(Instant::now() + Duration::from_millis(2));
                        }
                        let release_ms = recovery_started.elapsed().as_millis();
                        loop {
                            if worker_stop.load(Ordering::Acquire) {
                                anyhow::bail!("capture stopped while recovering");
                            }
                            let next = prepared.capture_target();
                            // A UAC prompt or the lock screen switches the input
                            // desktop; duplication must be made on that desktop.
                            butterpollo_windows::input::follow_input_desktop();
                            let opened = Capture::open_for_stream_reported(&next.0, &kind, hdr, &capture_config, capture_warnings.clone());
                            match opened {
                                Ok(mut recovered) => {
                                    if next != *target {
                                        tracing::info!(output = %next.0, "capture moved to the recreated display");
                                    }
                                    if let Some(pointer) = pointer.take()
                                        && let Err(error) = recovered.resume_pointer(pointer)
                                    {
                                        tracing::warn!(%error, "the pointer appears once it moves or changes");
                                    }
                                    tracing::info!(release_ms, elapsed_ms=recovery_started.elapsed().as_millis(), backend=recovered.backend(), "capture reopened after resource release");
                                    return Ok(recovered);
                                }
                                Err(error) if Instant::now() >= deadline => return Err(error),
                                Err(_) => thread::sleep(RECOVERY_RETRY),
                            }
                        }
                    };
                    let _ = started_tx.send(Ok(()));
                    let mut check_target = Instant::now();
                    let mut user_desktop = kind == "wgc"
                        && butterpollo_windows::capture::wgc_desktop_available();
                    let poll_interval = Duration::from_micros(
                        capture_config.integer("capture_poll_interval_us", 500).clamp(100, 1000) as u64,
                    );
                    while !worker_stop.load(Ordering::Acquire) {
                        if Instant::now() >= check_target {
                            check_target = Instant::now() + Duration::from_millis(100);
                            let next = prepared.capture_target();
                            let available = kind == "wgc"
                                && butterpollo_windows::capture::wgc_desktop_available();
                            let return_to_wgc = available && !user_desktop && capture.backend() == "ddx";
                            user_desktop = available;
                            if next != target || return_to_wgc {
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
                        // Reset before the helper's announcements are read: one
                        // that comes after them ends the wait below.
                        if let Some(signal) = capture.frame_signal() {
                            signal.reset()?;
                        }
                        match capture.next_gpu() {
                            Ok(Some(image)) => {
                                capture_warnings.clear("capture_recovery");
                                let captured = image.captured;
                                worker.publish_captured(Arc::new(image), captured)?;
                            }
                            Ok(None) => {
                                let interval = if capture_config.boolean("capture_predictive_poll", false)
                                    && capture.backend() == "ddx" {
                                    worker.poll_interval(poll_interval)
                                } else { poll_interval };
                                let deadline = Instant::now() + interval;
                                let until = capture.publication_deadline().unwrap_or(deadline).min(deadline);
                                match capture.frame_signal() {
                                    // The helper's frame wakes this at once instead
                                    // of at the next poll, up to 0.5 ms later.
                                    Some(signal) => {
                                        timer.until_or_signal(until, signal)?;
                                    }
                                    None => timer.until(until),
                                }
                            }
                            Err(e) => {
                                capture_warnings.set("capture_recovery", format!("Capture interrupted ({e:#}); reopening capture, with a frozen picture until frames resume. If this repeats, keep the display mode stable and check the WGC helper and graphics driver."));
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
                    let _ = worker.fail(format!("{e:#}"));
                    tracing::error!(error=%format!("{e:#}"),"capture worker stopped");
                }
            })?;
        let latest = Arc::new(Source {
            warnings,
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
                        .map(|p| p.downcast::<crate::display_session::StreamPreparation>())
                        .transpose()
                        .map_err(|_| anyhow::anyhow!("invalid launch preparation"))?;
                    let stream_preparation = match initial {
                        Some(p) if p.display.matches(&s.config) => *p,
                        previous => {
                            drop(previous);
                            h.app_display.lock().unwrap().remove(&s.launch.client.uuid);
                            crate::display_session::Ready::prepare(
                                crate::display_session::Prepared::create(
                                    &h, &s.launch, &s.config, &c,
                                )?,
                            )?
                        }
                    };
                    let prepared = stream_preparation.display.clone();
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
                    *s.capture_warnings.write().unwrap() = latest.warnings.clone();
                    let mut capture_wake = latest.subscribe()?;
                    let first = {
                        let deadline = Instant::now() + Duration::from_secs(10);
                        loop {
                            // No frame, encoder or filter is owned during startup.
                            latest.release_generation(&capture_wake);
                            if let Some(image) = latest.current()? { break image; }
                            if s.stopping() || h.stop.load(Ordering::Acquire) {
                                return Ok(());
                            }
                            if Instant::now() >= deadline {
                                anyhow::bail!("capture produced no GPU frame on {} using {} within 10 seconds; check that the selected display is powered on, or select a virtual display for headless streaming", prepared.output(), prepared.capture());
                            }
                            if let Some(image) = latest.wait_for_frame(&timer, &capture_wake, (Instant::now() + Duration::from_millis(50)).min(deadline))? { break image; }
                        }
                    };
                    let mut encoder = Some(Encoder::new_gpu_reported(
                        &s.config,
                        c.get("encoder", "auto"),
                        &first,
                        &c,
                        &s.launch.warnings,
                    )?);
                    *s.encoder.write().unwrap() = encoder.as_ref().unwrap().backend().into();
                    // A rebuilt encoder keeps the hardware family the stream
                    // started with: while a GPU recovers, "auto" would fall
                    // through to a software encoder and stay there.
                    let pinned_backend = encoder
                        .as_ref()
                        .filter(|e| e.hardware())
                        .map(Encoder::backend);
                    let source_refresh_hz = butterpollo_windows::display::mode(&first.gpu.display.display_name)
                        .ok().map(|mode| mode.dmDisplayFrequency);
                    tracing::info!(width=s.config.width,height=s.config.height,fps=f64::from(s.config.fps_millihz())/1000.,codec=s.config.codec,hdr=s.config.hdr,full_range=s.config.full_range(),color_matrix=s.config.color_matrix(),vrr=s.config.vrr_low_latency,requested_capture=%prepared.capture(),requested_encoder=c.get("encoder","auto"),encoder=encoder.as_ref().unwrap().backend(),adapter=%first.gpu.display.adapter,source_width=first.width,source_height=first.height,source_refresh_hz,source_pixel=?first.pixel,"stream configured");
                    let minimum = butterpollo_core::pyrowave::minimum_kbps(s.config.width, s.config.height, s.config.fps_millihz());
                    let recommended = butterpollo_core::pyrowave::recommended_kbps(s.config.width, s.config.height, s.config.fps_millihz());
                    let bitrate = s.bitrate.load(Ordering::Relaxed);
                    if s.config.codec == 3 && bitrate < minimum {
                        tracing::warn!(bitrate_kbps = bitrate, minimum_kbps = minimum, recommended_kbps = recommended, "PyroWave bitrate is too low: severe detail loss is likely. Raise the bitrate in Moonlight with network headroom, or use HEVC or AV1");
                    } else if s.config.codec == 3 && bitrate < recommended {
                        tracing::warn!(bitrate_kbps = bitrate, minimum_kbps = minimum, recommended_kbps = recommended, "PyroWave bitrate is below recommended: text and textures may lose detail. Quality depends on the picture; raise the bitrate in Moonlight with network headroom, or use HEVC or AV1");
                    }
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
                    // VRR claims each frame as it arrives, but no faster than the
                    // stream rate. The encoder budgets every frame from that rate,
                    // and the VRR virtual display runs at 1000 Hz: uncapped, a game
                    // or desktop faster than the stream starved every frame of
                    // bits (a blurry picture, fringed text) and flooded the link.
                    // The predictive waits stay off; they hold frames for a cadence.
                    let vrr = s.config.vrr_low_latency;
                    let arrival_pacing = vrr
                        || butterpollo_core::stream_policy::Pacing::from_config(&c) == butterpollo_core::stream_policy::Pacing::Arrival;
                    let mut pacer = butterpollo_core::stream_policy::Pacer::new(Instant::now(), period)
                        .with_prediction(!vrr && c.boolean("frame_pacing_predictive", true))
                        .with_source_phase(!vrr && c.boolean("frame_pacing_source_phase", prepared.capture() == "wgc"))
                        .with_spacing(if vrr { 0.5 } else { 0.75 });
                    let due = cadence.deadline();
                    let mut last_stamp = start;
                    let mut live_at = due;
                    let mut rebuild_encoder = false;
                    // Since when encoding has failed without a frame getting through.
                    let mut encoder_failing: Option<Instant> = None;
                    // Separate encoder failures in this session, each counted once.
                    let (mut failures, mut counted_failure) = (0u32, None);
                    let mut runtime_config = c.clone();
                    let mut profiles = None;
                    let mut foreground = None;
                    // The fullscreen game on this display, for quitting a game
                    // that a store client started outside the app's processes.
                    let mut quit_scan: Option<butterpollo_windows::foreground::Tracker> = None;
                    let mut quit_scan_due = Instant::now() + Duration::from_secs(1);
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
                    // Keep the legacy age split beside WGC's signed raw stamp
                    // offset: a future stamp must not look like instant delivery.
                    let mut claim_ages: Vec<(u64, u64)> = Vec::with_capacity(1024);
                    let mut wgc_stamp_ages: Vec<f64> = Vec::with_capacity(1024);
                    // The first pacing decision for the newest fresh frame, kept
                    // for the per-claim trace.
                    let mut first_seen: Option<(usize, Instant, Option<Duration>, Option<Instant>)> = None;
                    // Since when a full encoder has returned nothing.
                    let mut backlog_since: Option<Instant> = None;
                    let mut video_qos = Tagged::default();
                    let mut batch = butterpollo_windows::net::Batch::default();
                    let mut network_pacer = butterpollo_core::network_pacing::Pacer::new(Instant::now());
                    let mut link = None;
                    let mut link_due = Instant::now();
                    let batch_kb = match c.integer("video_max_batch_size_kb", 64) {
                        16 => 16,
                        32 => 32,
                        _ => 64,
                    };
                    let mut send_frames = |output: Vec<butterpollo_windows::encoder::Encoded>,
                                           peer: std::net::SocketAddr,
                                           call_latency: Duration|
                     -> Result<()> {
                        if !output.is_empty() { s.launch.warnings.clear("encoder_recovery"); }
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
                            // Wrap like the previous host; a saturating cast
                            // froze the clock after 13.25 hours.
                            let timestamp = (stamp.saturating_duration_since(start).as_secs_f64() * 90000.) as u64 as u32;
                            if frame.bytes.is_empty() {
                                continue;
                            }
                            // A frame beyond Moonlight's packet limit (very high
                            // bitrates) costs that frame and a keyframe, not the
                            // session.
                            let packets = match packetizer.encode_recovery(&frame.bytes,frame.idr,frame.after_invalidation,timestamp,processing) {
                                Ok(packets) => packets,
                                Err(error) => {
                                    tracing::warn!(error = %format!("{error:#}"), bytes = frame.bytes.len(), "encoded frame dropped");
                                    s.request_idr();
                                    continue;
                                }
                            };
                            next_wire_frame.set(u64::from(packetizer.frame));
                            let frame_bytes = packets.iter().map(|p|p.len() as u64).sum();
                            let bps = butterpollo_core::network_pacing::rate_bps(
                                c.integer("pacing_max_bitrate_kbps", 0),
                                s.bitrate.load(Ordering::Relaxed),
                                *link.get_or_insert_with(|| butterpollo_windows::net::routed_link_bps(peer)),
                                peer.ip().to_canonical().is_loopback(),
                            );
                            let mut remaining = packets.as_slice();
                            while !remaining.is_empty() {
                                if s.stopping() || h.stop.load(Ordering::Acquire) {
                                    return Ok(());
                                }
                                let now = Instant::now();
                                if network_pacer.due() > now {
                                    timer.until_precise(network_pacer.due());
                                }
                                let budget = (bps / 4000)
                                    .clamp(remaining[0].len() as u64, batch_kb * 1024)
                                    as usize;
                                let count =
                                    butterpollo_windows::net::Batch::count(remaining, budget);
                                let bytes = batch.send(&m.video, &remaining[..count], peer)?;
                                remaining = &remaining[count..];
                                network_pacer.sent(Instant::now(), bytes, if bytes > 0 { count } else { 0 }, peer.is_ipv6(), bps);
                                s.stats.packets.fetch_add(count as u64, Ordering::Relaxed);
                                s.stats.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
                            }
                            s.stats.frames.fetch_add(1, Ordering::Relaxed);
                            let sent = Instant::now();
                            s.stats.performance.lock().unwrap().record_timing(sent,butterpollo_core::performance::Timing{encode:latency,host:processing,age,sent:micros(sent.saturating_duration_since(claimed))},frame_bytes);
                            // The interface lookup takes a moment: refresh the
                            // link speed after the frame is out, for the next one.
                            if Instant::now() >= link_due {
                                link = Some(butterpollo_windows::net::routed_link_bps(peer));
                                link_due = Instant::now() + Duration::from_secs(2);
                            }
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
                                    *s.capture_warnings.write().unwrap() = latest.warnings.clone();
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
                                    wgc_stamp_ages.sort_by(f64::total_cmp);
                                    let wgc_stamp_frames = wgc_stamp_ages.len();
                                    let wgc_stamp_future_frames = wgc_stamp_ages.iter().filter(|age| **age < 0.).count();
                                    let wgc_stamp_to_host_mean_ms = (wgc_stamp_frames > 0).then(|| wgc_stamp_ages.iter().sum::<f64>() / wgc_stamp_frames as f64);
                                    let wgc_stamp_to_host_p95_ms = wgc_stamp_ages.get(wgc_stamp_frames.saturating_sub(1) * 95 / 100).copied();
                                    wgc_stamp_ages.clear();

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
                                        wgc_stamp_to_host_mean_ms,
                                        wgc_stamp_to_host_p95_ms,
                                        wgc_stamp_frames,
                                        wgc_stamp_future_frames,

                                        send_interval_p95_ms=timing["send_interval_p95_ms"].as_f64().unwrap_or(0.),
                                        send_interval_p99_ms=timing["send_interval_p99_ms"].as_f64().unwrap_or(0.),
                                        send_interval_max_ms=timing["send_interval_max_ms"].as_f64().unwrap_or(0.),
                                        // Keep reference feedback distinct from IDR recovery.
                                        idr_requests=s.stats.idr_requests.load(Ordering::Relaxed),
                                        reference_invalidations=s.stats.reference_invalidations.load(Ordering::Relaxed),
                                        bitrate_kbps=s.bitrate.load(Ordering::Relaxed),
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
                            if s.config.video_qos {
                                video_qos.follow(&m.video, peer, false);
                            }
                            let now = Instant::now();
                            let due = cadence.deadline();
                            if !arrival_pacing && now < due {
                                while Instant::now() < due {
                                    if encoder.as_ref().is_some_and(Encoder::pending) {
                                        send_frames(collect(&mut encoder, &mut encoder_failing, &s.launch.warnings)?, peer, Duration::ZERO)?;
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
                                pacer.reset_source_phase();
                                latest.release_generation(&capture_wake);
                                continue;
                            };
                            // Resolution changes or DXGI loss can recreate the capture device.
                            rebuild_encoder |= encoder
                                .as_ref()
                                .is_none_or(|encoder| !encoder.accepts_gpu_device(&image));
                            if rebuild_encoder {
                                pacer.reset_source_phase();
                            }
                            if arrival_pacing {
                                pacer.observe_source(image.captured);
                            }
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
                                // A poll can block for the encoder's 1 ms query
                                // timeout; close to the claim it would overshoot.
                                if encoder.as_ref().is_some_and(Encoder::pending)
                                    && deadline.saturating_duration_since(Instant::now()) >= Duration::from_millis(1)
                                {
                                    send_frames(collect(&mut encoder, &mut encoder_failing, &s.launch.warnings)?, peer, Duration::ZERO)?;
                                }
                                let until = if encoder.as_ref().is_some_and(Encoder::pending) {
                                    deadline.min(Instant::now() + OUTPUT_POLL)
                                } else {
                                    deadline
                                };
                                // The claim itself is met precisely; a poll for the
                                // encoder's output is not worth spinning for.
                                if until == deadline {
                                    latest.wait_if_current_precise(&timer, &capture_wake, &image, until)?;
                                } else {
                                    latest.wait_if_current(&timer, &capture_wake, &image, until)?;
                                }
                                continue;
                            }
                            let repeat_due = if arrival_pacing {
                                pacer.repeat_deadline(encoded_at + static_period, image.captured, latest.source_interval())
                            } else {
                                encoded_at + static_period
                            };
                            if !rebuild_encoder
                                && (s.config.vrr_low_latency || limit_static_rate || arrival_pacing)
                                && !s.idr.load(Ordering::Acquire)
                                && s.invalidation.lock().unwrap().is_none()
                                && last_image
                                    .as_ref()
                                    .is_some_and(|previous| Arc::ptr_eq(previous, &image))
                                && Instant::now() < repeat_due
                            {
                                if encoder.as_ref().is_some_and(Encoder::pending) { send_frames(collect(&mut encoder, &mut encoder_failing, &s.launch.warnings)?, peer, Duration::ZERO)?; }
                                let wait = if encoder.as_ref().is_some_and(Encoder::pending) { OUTPUT_POLL } else { period };
                                latest.wait_if_current(&timer, &capture_wake, &image, Instant::now() + wait.min(repeat_due.saturating_duration_since(Instant::now())))?;
                                continue;
                            }
                            // A picture claimed while the encoder is behind only
                            // waits in its queue: take its output first, then
                            // claim the newest picture.
                            if !rebuild_encoder
                                && encoder.as_ref().is_some_and(|e| e.backlog() >= ENCODER_BACKLOG)
                            {
                                // An encoder that returns nothing for 100 ms is
                                // recreated, as a queue that never drained was.
                                if backlog_since.get_or_insert_with(Instant::now).elapsed() >= Duration::from_millis(100) {
                                    backlog_since = None;
                                    let since = *encoder_failing.get_or_insert_with(Instant::now);
                                    if since.elapsed() >= ENCODER_RECOVERY {
                                        anyhow::bail!("the encoder stopped returning frames");
                                    }
                                    s.launch.warnings.set("encoder_recovery", "The encoder returned no frame for 100 ms; recreating the same encoder while the picture freezes. Lower game GPU load or update the graphics driver if this repeats.");
                                    encoder = None;
                                    continue;
                                }
                                send_frames(collect(&mut encoder, &mut encoder_failing, &s.launch.warnings)?, peer, Duration::ZERO)?;
                                continue;
                            }
                            backlog_since = None;
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
                            if s.launch.role == Role::Stream && Instant::now() >= quit_scan_due {
                                quit_scan_due = Instant::now() + Duration::from_secs(1);
                                // The scan runs on its own thread; this reads its last result.
                                if let Some((program, pid)) = quit_scan
                                    .get_or_insert_with(butterpollo_windows::foreground::Tracker::default)
                                    .poll_process(&[], &image.gpu.display)
                                    && let Some(app) = h.current_app.lock().unwrap().as_mut()
                                    // Only the stream of the client that launched the app:
                                    // another client's display shows its own programs.
                                    // An app started from the console has no owner.
                                    && (app.owner.is_empty() || app.owner == s.launch.client.uuid)
                                {
                                    app.observe_foreground(pid, &program);
                                }
                            }
                            let rebuilt = rebuild_encoder;
                            if rebuild_encoder {
                                encoder = None;
                                // Some drivers accept the compute path at creation and
                                // fail on it later, every time: after a second failure
                                // in the session, convert on the graphics queue. One
                                // failure (a game holding the GPU, a driver reset) must
                                // not cost the rest of the session the slower path,
                                // 6-9 ms a frame beside a GPU-bound game.
                                if let Some(since) = encoder_failing
                                    && counted_failure != Some(since)
                                {
                                    counted_failure = Some(since);
                                    failures += 1;
                                }
                                let mut tuning = c.clone();
                                if failures >= 2 {
                                    tuning.values.insert("gpu_compute_conversion".into(), "false".into());
                                    if butterpollo_windows::compute::enabled(&c) && matches!(pinned_backend, Some("amf" | "pyrowave")) {
                                        s.launch.warnings.set("encoder_compute_recovery", "Compute conversion disabled after repeated encoder failures; using the graphics queue for the rest of this session. A busy game can delay frames; lower game GPU load or update the AMD driver, then reconnect to retry compute.");
                                    }
                                }
                                match Encoder::new_gpu_reported(
                                    &s.config,
                                    pinned_backend.unwrap_or(c.get("encoder", "auto")),
                                    &image,
                                    &tuning,
                                    &s.launch.warnings,
                                ) {
                                    Ok(created) => {
                                        *s.encoder.write().unwrap() = created.backend().into();
                                        encoder = Some(created);
                                    },
                                    Err(error) => {
                                        let since = *encoder_failing.get_or_insert_with(Instant::now);
                                        if since.elapsed() >= ENCODER_RECOVERY {
                                            return Err(error.context("the encoder could not be recreated"));
                                        }
                                        s.launch.warnings.set("encoder_recovery", format!("Encoder recreation failed ({error:#}); retrying the same backend while the picture freezes. Check the driver and lower game GPU load; the session will fail if frames do not resume."));
                                        timer.until(Instant::now() + Duration::from_millis(100));
                                        continue;
                                    }
                                }
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
                            if tracing::enabled!(target: "pacing", tracing::Level::TRACE) {
                                let us = |at: Instant| at.saturating_duration_since(start).as_micros() as u64;
                                let key = Arc::as_ptr(&image) as usize;
                                let (seen, interval, deadline) = match first_seen {
                                    Some((k, seen, interval, deadline)) if k == key => (seen, interval, deadline),
                                    _ => (begin, latest.source_interval(), None),
                                };
                                tracing::trace!(
                                    target: "pacing",
                                    capture_id = latest.trace_publication_id(&image).unwrap_or(0),
                                    presented = us(image.captured),
                                    acquired = us(image.acquired),
                                    seen = us(seen),
                                    claim = us(begin),
                                    interval = interval.map_or(0, |i| i.as_micros() as u64),
                                    deadline = deadline.map_or(0, us),
                                    source_id = latest.trace_source_id(),
                                    stream_id = %s.launch.id,
                                    fresh,
                                    "claim"
                                );
                            }
                            if fresh && claim_ages.len() < 4096 {
                                let micros = |d: Duration| d.as_micros().min(u128::from(u64::MAX)) as u64;
                                claim_ages.push((
                                    micros(image.acquired.saturating_duration_since(image.captured)),
                                    micros(begin.saturating_duration_since(image.acquired)),
                                ));
                                if let Some(stamp) = image.wgc_stamp {
                                    let age = if image.acquired >= stamp {
                                        image.acquired.duration_since(stamp).as_secs_f64()
                                    } else {
                                        -stamp.duration_since(image.acquired).as_secs_f64()
                                    };
                                    wgc_stamp_ages.push(age * 1000.);
                                }
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
                            let encoded = (|| -> Result<Vec<butterpollo_windows::encoder::Encoded>> {
                                Ok(if let Ok(Some(transformed)) = transformed.as_ref() {
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
                                })
                            })();
                            let output = match encoded {
                                Ok(output) => {
                                    encoder_progress(&mut encoder_failing, !output.is_empty(), Instant::now())?;
                                    output
                                }
                                Err(error) => {
                                    // A stalled or reset GPU costs these frames and a
                                    // keyframe; the rebuild requests it. Only failures
                                    // that keep coming end the session.
                                    let since = *encoder_failing.get_or_insert_with(Instant::now);
                                    if since.elapsed() >= ENCODER_RECOVERY {
                                        return Err(error.context("the encoder kept failing"));
                                    }
                                    s.launch.warnings.set("encoder_recovery", format!("Encoding failed ({error:#}); recreating the same encoder while the picture freezes. Lower game GPU load or update the graphics driver if this repeats."));
                                    rebuild_encoder = true;
                                    continue;
                                }
                            };
                            let call_latency = begin.elapsed();
                            // Repeat deadlines start at submission, so encoder work
                            // does not extend the interval between static frames.
                            encoded_at = begin;
                            if arrival_pacing {
                                // Only a new picture spends pacing credit. A picture
                                // encoded again (a keyframe the client asked for, a
                                // static repeat) must not hold back the next game
                                // frame, as the C++ host never does.
                                if fresh {
                                    pacer.claimed(begin);
                                }
                                latest.grid.lock().unwrap().anchor = pacer.allowed_at(begin);
                            } else {
                                cadence.submitted(begin);
                                latest.grid.lock().unwrap().anchor = cadence.deadline();
                            }
                            last_image = Some(image);
                            // An allocation can later reuse this image's address;
                            // its trace observation belongs only to this submission.
                            first_seen = None;
                            send_frames(output, peer, call_latency)?;
                        }
                        Ok(())
                    })();
                    // Fail before stopping, so the control stream tells the
                    // client about the error instead of a normal close.
                    if result.is_err() {
                        s.fail();
                    } else {
                        s.stop();
                    }
                    let _ = audio.join();
                    result
                })();
                if let Err(e) = result {
                    s.fail();
                    tracing::error!(error=%format!("{e:#}"),client=%s.launch.client.name,"session failed; check the reported encoder, capture or socket error before reconnecting");
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
        let interval = Duration::from_millis(u64::from(s.config.audio_packet_ms));
        // When audio last arrived, and when an idle endpoint's next silence is due.
        let mut heard = Instant::now();
        let mut audio_qos = Tagged::default();
        let mut loss = butterpollo_core::audio::HostLoss::default();
        let mut next = Instant::now();
        let timer = butterpollo_windows::timing::Timer::new()?;
        let silence = vec![0.; frames * s.config.audio_channels as usize];
        while !s.stopping() && !h.stop.load(Ordering::Acquire) {
            match capture
                .as_ref()
                .filter(|capturing| capturing.event_driven())
            {
                // Woken the moment audio is captured. An idle endpoint sends
                // no events: the wait ends at its next silence, at most 5 ms.
                Some(capturing) => capturing.wait(
                    next.saturating_duration_since(Instant::now())
                        .clamp(Duration::from_millis(1), Duration::from_millis(5)),
                ),
                None => timer.until(Instant::now() + Duration::from_millis(1)),
            }
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
                                tracing::info!(
                                    channels = s.config.audio_channels,
                                    event_driven = value.event_driven(),
                                    "WASAPI audio capture started"
                                );
                                capture = Some(value);
                                capture_failed = false;
                                audio_error = None;
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
            // Every whole packet the endpoint has delivered goes out at once, as
            // Sunshine sends them. On a fixed tick each waited up to a packet,
            // and a tick just before a late chunk sent silence in its place.
            let mut packets = Vec::new();
            if let Some(capturing) = capture.as_mut()
                && let Some(waited) = capturing.since_drained()
            {
                loss.read_after(waited, capturing.buffer());
            }
            while let Some(capturing) = capture.as_mut() {
                match capturing.read(frames) {
                    Ok(Some(samples)) => packets.push(samples),
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(%error, "audio endpoint changed; reopening WASAPI");
                        capture = None;
                        capture_failed = true;
                    }
                }
            }
            let now = Instant::now();
            if !packets.is_empty() {
                heard = now;
                next = now + interval;
            } else if now >= next
                && now.saturating_duration_since(heard) >= Duration::from_millis(50)
            {
                // WASAPI delivers roughly 10 ms chunks; allow for scheduling jitter
                // before treating a quiet capture queue as an idle endpoint.
                packets.push(silence.clone());
                next += interval;
                if next < now {
                    next = now + interval;
                }
            }
            if let Some(peer) = peer {
                if s.config.audio_qos {
                    audio_qos.follow(&self.audio, peer, true);
                }
                for samples in &packets {
                    for packet in p.encode(&opus.encode(samples)?)? {
                        // A lost audio packet is concealed by the client; only a
                        // broken socket stops the audio.
                        if !butterpollo_windows::net::send_datagram(&self.audio, &packet, peer)? {
                            loss.unsent();
                        }
                    }
                }
            } else if start.elapsed() > crate::network::ping_timeout(&config) {
                anyhow::bail!("client audio ping timed out");
            }
            if let Some(report) = loss.report(Instant::now()) {
                let ms = |d: Duration| d.as_secs_f64() * 1000.;
                tracing::warn!(
                    late_reads = report.late_reads,
                    lost_ms = ms(report.lost),
                    longest_wait_ms = ms(report.longest),
                    buffer_ms = ms(report.buffer),
                    unsent_packets = report.unsent,
                    "audio lost on the host before sending"
                );
            }
        }
        Ok(())
    }
    fn control(&self, h: Shared) -> Result<()> {
        let socket = crate::network::udp((self.bind, self.control_port).into())?;
        butterpollo_windows::net::configure_udp(&socket)?;
        let raw_socket = std::os::windows::io::AsRawSocket::as_raw_socket(&socket);
        let mut host = Host::new(
            ControlSocket::new(socket),
            HostSettings {
                peer_limit: 32,
                // Moonlight opens 48 channels (gamepads from 0x10, motion
                // from 0x20) and folds those above the host's limit onto
                // channel 0, where one lost ping holds up all controller
                // input. The previous host allowed the protocol maximum.
                channel_limit: 255,
                ..Default::default()
            },
        )?;
        let _com = ComGuard::new()?;
        let _priority = Priority::input();
        let mut peers: HashMap<PeerID, ControlPeer> = HashMap::new();
        let input_timer = butterpollo_windows::timing::Timer::new()?;
        let mut feedback_at = Instant::now();
        while !h.stop.load(Ordering::Acquire) {
            let ping_timeout = crate::network::ping_timeout(&h.config.read().unwrap());
            let mut disconnected = Vec::new();
            // Acknowledgements and anything else ENet sends wait until this
            // pass's input is applied.
            host.socket_mut().hold();
            // Bound each pass so a busy input peer cannot starve cleanup or feedback.
            for _ in 0..512 {
                let Some(event) = host.service()? else { break };
                match event {
                    Event::Connect { peer, data } => {
                        let Some(address) = peer.address() else {
                            peer.disconnect_now(0);
                            continue;
                        };
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
                                    injector_tried: false,
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
                        let id = peer.id();
                        if let Some(p) = peers.remove(&id) {
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
                            disconnected.push(p);
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
                            // SS_RFI_REQUEST: first frame, a reserved word, last
                            // frame, reserved words; older clients sent two
                            // 64-bit indices, whose low words sit at the same
                            // offsets. Requiring 8 bytes turned every request
                            // into a keyframe.
                            0x0301 if payload.len() >= 12 => {
                                s.request_invalidation(
                                    u64::from(u32::from_le_bytes(payload[..4].try_into().unwrap())),
                                    u64::from(u32::from_le_bytes(
                                        payload[8..12].try_into().unwrap(),
                                    )),
                                );
                            }
                            0x0301 => s.request_invalidation(0, 0),
                            0x0302 => s.request_idr(),
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
                    let timed_out = p.seen.elapsed() > ping_timeout;
                    if s.stopping() || timed_out {
                        if timed_out && !s.stopping() {
                            tracing::warn!(client=%s.launch.client.name,"client control stream timed out");
                            s.fail();
                        }
                        let reason = s.termination_reason();
                        if let Ok(message) = p.encrypt(&s, 0x0109, &reason.to_be_bytes()) {
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
                    // Made before input comes; after a failure, again when it
                    // does. Making it with the first input held that input up.
                    if p.injector.is_none() && (!p.inputs.is_empty() || !p.injector_tried) {
                        p.injector_tried = true;
                        let c = match effective_config(&h, &s.launch) {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!(error = %format!("{e:#}"), "input configuration unavailable");
                                p.inputs.clear();
                                continue;
                            }
                        };
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
                            Ok(mut i) => {
                                i.set_stream_size(s.config.width, s.config.height);
                                p.injector = Some(i);
                            }
                            Err(e) => tracing::warn!(error=%e,"input initialization failed"),
                        }
                    }
                    if let Some(i) = &mut p.injector {
                        // The stream's display can be created or renamed after
                        // input began; absolute input follows it.
                        i.set_output(&s.output.read().unwrap());
                    }
                    let inputs = std::mem::take(&mut p.inputs);
                    if let Some(i) = &mut p.injector {
                        for e in i.apply_all(&inputs) {
                            tracing::debug!(error=%e,"input injection failed");
                        }
                    }
                    if !poll_feedback
                        && let Some(i) = &mut p.injector
                        && let Err(e) = i.due()
                    {
                        tracing::debug!(error=%e,"input release or repeat failed");
                    }
                    if poll_feedback
                        && let Some(i) = &mut p.injector
                        && let Err(e) = i.refresh()
                    {
                        tracing::debug!(error=%e,"pointer refresh failed");
                    }
                    // The gamepad thread polls feedback every 8 ms; pass on
                    // what it found, and the sensors an arrived pad has.
                    let mut messages = Vec::new();
                    if let Some(i) = &p.injector {
                        for report in i.gamepad_reports() {
                            match report {
                                PadReport::Feedback { id, kind, data } => messages.extend(
                                    feedback_packets(id, kind, &data)
                                        .into_iter()
                                        .filter(|(kind, _)| i.feedback_allowed(*kind)),
                                ),
                                PadReport::Motion { id, capabilities } => {
                                    messages.extend(motion_requests(id, capabilities))
                                }
                            }
                        }
                    }
                    for (kind, payload) in messages {
                        if let Ok(message) = p.encrypt(&s, kind, &payload) {
                            let _ = host
                                .peer_mut(*peer_id)
                                .send(1, &Packet::new(message, PacketKind::Reliable));
                        }
                    }
                } else if !pending || p.seen.elapsed() > ping_timeout {
                    host.peer_mut(*peer_id).disconnect_now(0);
                    remove.push(*peer_id);
                }
            }
            host.socket_mut().release()?;
            // Unplugging gamepads can wait; finish this pass's input and sends first.
            drop(disconnected);
            for peer in remove {
                peers.remove(&peer);
            }
            host.flush();
            // Wake as soon as input arrives; otherwise within a millisecond for
            // feedback and cleanup. A fixed sleep made every event wait for it.
            if butterpollo_windows::net::wait_readable(raw_socket, 1).is_err() {
                // A polling error must not spin the loop.
                input_timer.until(Instant::now() + Duration::from_millis(1));
            }
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
    /// Whether making the injector was tried.
    injector_tried: bool,
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
/// Ask the client for an arrived pad's accelerometer (kind 1) and gyroscope
/// (kind 2) at 200 Hz, when it has them.
fn motion_requests(id: u8, capabilities: u16) -> Vec<(u16, Vec<u8>)> {
    [(0x10, 1), (0x20, 2)]
        .into_iter()
        .filter(|(sensor, _)| capabilities & sensor != 0)
        .map(|(_, kind)| {
            let mut payload = u16::from(id).to_le_bytes().to_vec();
            payload.extend_from_slice(&200u16.to_le_bytes());
            payload.push(kind);
            (0x5501, payload)
        })
        .collect()
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
            // moonlight-common-c: id, event flags (0x08 left, 0x04 right),
            // left type, right type, then each trigger's ten parameters.
            // The driver reports both triggers at once.
            let mut trigger = id.to_le_bytes().to_vec();
            trigger.extend_from_slice(&[0x0c, data[10], data[21]]);
            trigger.extend_from_slice(&data[11..21]);
            trigger.extend_from_slice(&data[22..32]);
            result.push((0x5503, trigger));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vigem_feedback_forwards_rumble_and_only_ds4_lightbar() {
        let report = [255, 255, 128, 128, 12, 34, 56, 0];
        let x360 = feedback_packets(2, 3, &report);
        assert_eq!(
            x360,
            vec![(0x010b, vec![0xee, 0xff, 0xc0, 0, 2, 0, 255, 255, 128, 128])]
        );
        let ds4 = feedback_packets(2, 1, &report);
        assert_eq!(ds4[0], x360[0]);
        assert_eq!(ds4[1], (0x5502, vec![2, 0, 12, 34, 56]));
        assert_eq!(ds4.len(), 2);
    }
    #[test]
    fn an_override_the_host_cannot_use_is_skipped_and_the_rest_apply() {
        let mut config = Config::default();
        let overrides =
            serde_json::json!({"frame_limiter_fps_limit": "-1", "fec_percentage": "30"});
        apply_overrides(&mut config, overrides.as_object().unwrap()).unwrap();
        assert_eq!(config.get("fec_percentage", ""), "30");
        assert!(!config.values.contains_key("frame_limiter_fps_limit"));
    }
    #[test]
    fn accepting_input_without_output_cannot_extend_encoder_recovery() {
        let start = Instant::now();
        let mut failing = Some(start);
        for seconds in 0..5 {
            encoder_progress(&mut failing, false, start + Duration::from_secs(seconds)).unwrap();
            assert_eq!(failing, Some(start));
        }
        assert!(encoder_progress(&mut failing, false, start + ENCODER_RECOVERY).is_err());
    }
    #[test]
    fn completed_output_ends_recovery_and_allows_a_later_independent_failure() {
        let start = Instant::now();
        let mut failing = Some(start);
        encoder_progress(&mut failing, true, start + Duration::from_secs(4)).unwrap();
        assert_eq!(failing, None);
        encoder_progress(&mut failing, false, start + ENCODER_RECOVERY).unwrap();
        failing = Some(start + Duration::from_secs(6));
        encoder_progress(&mut failing, false, start + Duration::from_secs(7)).unwrap();
        assert!(encoder_progress(&mut failing, false, start + Duration::from_secs(11)).is_err());
    }
    #[test]
    fn wgc_interval_is_unrestricted_by_default_and_preserves_overrides() {
        let default = Config::default();
        assert!(!capture_config(&default).boolean("wgc_high_rate_capture", true));
        for (setting, expected) in [("true", true), ("false", false)] {
            let config = Config::parse(&format!("wgc_high_rate_capture = {setting}")).unwrap();
            let original = config.values.clone();
            assert_eq!(
                capture_config(&config).boolean("wgc_high_rate_capture", !expected),
                expected
            );
            assert_eq!(config.values, original);
        }
        assert!(!default.values.contains_key("wgc_high_rate_capture"));
    }
    #[test]
    fn shared_capture_separates_explicit_wgc_intervals_without_splitting_ddx() {
        let key = |kind, setting: &str| {
            CaptureKey::new(
                kind,
                "display",
                false,
                &capture_config(&Config::parse(setting).unwrap()),
                "",
            )
        };
        for kind in ["wgc", "auto"] {
            assert_eq!(key(kind, ""), key(kind, "wgc_high_rate_capture=false"));
            assert_ne!(key(kind, ""), key(kind, "wgc_high_rate_capture=true"));
            assert_ne!(key(kind, ""), key(kind, "wgc_drain_to_newest=true"));
            assert_ne!(key(kind, ""), key(kind, "wgc_helper_streaming_scope=true"));
        }
        for kind in ["ddx", "dxgi"] {
            assert_eq!(key(kind, ""), key(kind, "wgc_high_rate_capture=true"));
            assert_eq!(key(kind, ""), key(kind, "wgc_high_rate_capture=false"));
            assert_eq!(key(kind, ""), key(kind, "wgc_drain_to_newest=true"));
            assert_eq!(key(kind, ""), key(kind, "wgc_helper_streaming_scope=true"));
        }
    }
    #[test]
    fn shared_capture_respects_compute_overrides_and_independent_frame_phases() {
        let config = Config::default();
        let key =
            |config: &Config, phase: &str| CaptureKey::new("wgc", "display", false, config, phase);
        let default = key(&config, "first");
        assert_eq!(default, key(&config, "second"));
        let mut helper = config.clone();
        helper
            .values
            .insert("wgc_user_helper".into(), "true".into());
        assert_ne!(default, key(&helper, "first"));
        let mut changed = config.clone();
        changed
            .values
            .insert("wgc_compute_copy".into(), "false".into());
        assert_ne!(default, key(&changed, "first"));
        changed
            .values
            .insert("wgc_compute_copy".into(), "true".into());
        assert_eq!(default, key(&changed, "first"));
        changed
            .values
            .insert("gpu_compute_conversion".into(), "false".into());
        assert_ne!(default, key(&changed, "first"));
        changed
            .values
            .insert("wgc_slot_aligned_publish".into(), "true".into());
        assert_ne!(key(&changed, "first"), key(&changed, "second"));
    }
    #[test]
    fn dualsense_trigger_effects_use_the_moonlight_layout() {
        let mut data = vec![0u8; 32];
        data[..4].copy_from_slice(&[1, 2, 3, 4]);
        data[7] = 2;
        data[10] = 0x21;
        for (i, byte) in data[11..21].iter_mut().enumerate() {
            *byte = i as u8 + 1;
        }
        data[21] = 0x26;
        for (i, byte) in data[22..32].iter_mut().enumerate() {
            *byte = i as u8 + 11;
        }
        let packets = feedback_packets(1, 5, &data);
        let (_, trigger) = packets.iter().find(|(kind, _)| *kind == 0x5503).unwrap();
        let mut expected = vec![1, 0, 0x0c, 0x21, 0x26];
        expected.extend(1..=20);
        assert_eq!(trigger, &expected);
        assert!(packets.iter().any(|(kind, _)| *kind == 0x010b));
    }
    #[test]
    fn held_control_datagrams_go_out_in_order_once_released() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = receiver.local_addr().unwrap();
        let mut socket = ControlSocket::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let mut buffer = [0u8; 64];
        socket.hold();
        // The middle one is too large for UDP: it is lost, the rest go out.
        for datagram in [&b"one"[..], &[0; 70_000], b"two"] {
            assert_eq!(
                rusty_enet::Socket::send(&mut socket, address, datagram).unwrap(),
                datagram.len()
            );
        }
        receiver.set_nonblocking(true).unwrap();
        assert!(receiver.recv_from(&mut buffer).is_err());
        socket.release().unwrap();
        receiver.set_nonblocking(false).unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut received = Vec::new();
        for _ in 0..2 {
            let (length, _) = receiver.recv_from(&mut buffer).unwrap();
            received.push(buffer[..length].to_vec());
        }
        assert_eq!(received, [b"one".to_vec(), b"two".to_vec()]);
        // Released: sent at once again.
        rusty_enet::Socket::send(&mut socket, address, b"three").unwrap();
        let (length, _) = receiver.recv_from(&mut buffer).unwrap();
        assert_eq!(&buffer[..length], b"three");
    }
    #[test]
    fn an_arrived_pad_is_asked_for_the_sensors_it_has() {
        assert_eq!(
            motion_requests(3, 0x30),
            [
                (0x5501, vec![3, 0, 200, 0, 1]),
                (0x5501, vec![3, 0, 200, 0, 2])
            ]
        );
        assert_eq!(motion_requests(1, 0x20), [(0x5501, vec![1, 0, 200, 0, 2])]);
        assert!(motion_requests(1, 0x0f).is_empty());
    }
}
