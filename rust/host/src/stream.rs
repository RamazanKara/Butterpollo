use crate::state::{Launch, Session, Shared};
mod capture;
mod device_recovery;
#[cfg(test)]
mod encoder_tests;
#[cfg(test)]
pub(crate) mod reconnect_tests;
use anyhow::{Context, Result};
use butterpollo_core::{
    config::Config,
    crypto, input,
    packet::{AudioPacketizer, VideoPacketizer},
    session::Role,
};
use butterpollo_windows::device_loss::DeviceLost;
pub(crate) const RTX_KEYS: &[&str] = &[
    "rtx_hdr",
    "rtx_hdr_sdr_brightness",
    "rtx_hdr_contrast",
    "rtx_hdr_saturation",
    "rtx_hdr_middle_gray",
    "rtx_hdr_peak_brightness",
];
fn server_command(
    h: &Shared,
    client: &butterpollo_core::state::Client,
    payload: &[u8],
    command_at: &mut Option<Instant>,
) -> Option<(String, bool)> {
    if !client.allows(1 << 20) || payload.len() != 1 {
        return None;
    }
    if h.current_app
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|app| !app.allow_client_commands)
        || command_at.is_some_and(|last| last.elapsed() < Duration::from_secs(1))
    {
        return None;
    }
    *command_at = Some(Instant::now());
    let commands: serde_json::Value =
        serde_json::from_str(h.config.read().unwrap().get("server_cmd", "[]")).unwrap_or_default();
    let command = commands.as_array()?.get(payload[0] as usize)?;
    Some((
        command.get("cmd")?.as_str()?.to_owned(),
        command
            .get("elevated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    ))
}
/// How often completed encoder output is collected while a frame is in flight.
const OUTPUT_POLL: Duration = Duration::from_micros(100);
/// How long an encoder that fails mid-stream is recreated before the session
/// gives up: a GPU busy with a game or a driver reset costs frames, not the
/// stream. Moonlight keeps waiting with a frozen picture meanwhile.
const ENCODER_RECOVERY: Duration = Duration::from_secs(20);
/// Keep one queued picture while an encode is running. Larger queues did not
/// improve throughput on an overloaded AMF encoder, but increased frame age.
const ENCODER_BACKLOG: usize = 2;
fn encoder_progress(
    failing: &mut Option<Instant>,
    produced_frame: bool,
    now: Instant,
) -> Result<()> {
    if produced_frame {
        if let Some(since) = *failing {
            tracing::info!(
                recovery_ms = now.duration_since(since).as_millis(),
                "encoder output resumed"
            );
        }
        *failing = None;
    } else if failing.is_some_and(|since| now.duration_since(since) >= ENCODER_RECOVERY) {
        anyhow::bail!("the encoder did not produce a frame during recovery");
    }
    Ok(())
}
// Keep recovery testable without constructing a GPU or an AMF runtime.
trait EncoderOutput {
    fn poll(&mut self) -> Result<Vec<butterpollo_windows::encoder::Encoded>>;
    fn pending(&self) -> bool;
    fn backlog(&self) -> usize;
    fn log_stall(&self);
    fn device_removed(&self) -> Option<DeviceLost> {
        None
    }
}
impl EncoderOutput for Encoder {
    fn poll(&mut self) -> Result<Vec<butterpollo_windows::encoder::Encoded>> {
        self.poll()
    }
    fn pending(&self) -> bool {
        self.pending()
    }
    fn backlog(&self) -> usize {
        self.backlog()
    }
    fn log_stall(&self) {
        self.log_stall();
    }
    fn device_removed(&self) -> Option<DeviceLost> {
        self.device_removed()
    }
}
#[derive(Default)]
struct EncoderRecovery {
    failing: Option<Instant>,
    backlog_since: Option<Instant>,
    recreations: u32,
    device_loss: Option<DeviceLost>,
    #[cfg(any(debug_assertions, test))]
    stall: crate::soak_fault::EncoderStall,
}
impl EncoderRecovery {
    fn pending(&self, encoder: &impl EncoderOutput) -> bool {
        if self.device_loss.is_some() {
            return false;
        }
        #[cfg(any(debug_assertions, test))]
        if !self.stall.held.is_empty() {
            return true;
        }
        encoder.pending()
    }
    fn backlog(&self, encoder: &impl EncoderOutput) -> usize {
        let backlog = encoder.backlog();
        #[cfg(any(debug_assertions, test))]
        let backlog = backlog + self.stall.held.len();
        backlog
    }
    fn output(
        &mut self,
        output: Vec<butterpollo_windows::encoder::Encoded>,
        now: impl Fn() -> Instant,
    ) -> Result<Vec<butterpollo_windows::encoder::Encoded>> {
        #[cfg(any(debug_assertions, test))]
        let output = self.stall.output(output, now())?;
        encoder_progress(&mut self.failing, !output.is_empty(), now())?;
        Ok(output)
    }
    /// A recreated codec cannot send pictures retained from its predecessor.
    fn recreated(&mut self, session: &Session) {
        #[cfg(any(debug_assertions, test))]
        self.stall.held.clear();
        session.request_idr();
    }
    /// Output the encoder has finished. Only failures lasting the recovery
    /// budget end the stream; the next frame recreates a dropped encoder.
    fn collect(
        &mut self,
        encoder: &mut Option<impl EncoderOutput>,
        warnings: &butterpollo_core::session::Warnings,
        now: impl Fn() -> Instant,
    ) -> Result<Vec<butterpollo_windows::encoder::Encoded>> {
        let Some(active) = encoder.as_mut() else {
            return Ok(vec![]);
        };
        match active.poll() {
            Ok(output) => self.output(output, now),
            Err(error) => {
                if let Some(loss) = active
                    .device_removed()
                    .or_else(|| DeviceLost::from_error(&error))
                {
                    self.device_loss = Some(loss);
                    self.failing.get_or_insert_with(&now);
                    return Ok(vec![]);
                }
                let since = *self.failing.get_or_insert_with(&now);
                if now().duration_since(since) >= ENCODER_RECOVERY {
                    return Err(error.context("the encoder kept failing"));
                }
                warnings.set("encoder_recovery", format!("Encoder output failed ({error:#}); recreating the same encoder while the picture freezes. Lower game GPU load or update the graphics driver if this repeats."));
                active.log_stall();
                *encoder = None;
                Ok(vec![])
            }
        }
    }
    fn poll_full(
        &mut self,
        encoder: &mut Option<impl EncoderOutput>,
        warnings: &butterpollo_core::session::Warnings,
        now: impl Fn() -> Instant,
    ) -> Result<Option<Vec<butterpollo_windows::encoder::Encoded>>> {
        if self.failing.is_none() {
            self.recreations = 0;
        }
        let limit = butterpollo_core::stream_policy::encoder_stall_limit(self.recreations);
        let backlog_since = *self.backlog_since.get_or_insert_with(&now);
        if now().duration_since(backlog_since) >= limit {
            self.backlog_since = None;
            if let Some(loss) = encoder.as_ref().unwrap().device_removed() {
                self.device_loss = Some(loss);
                self.failing.get_or_insert_with(&now);
                return Ok(None);
            }
            let since = *self.failing.get_or_insert_with(&now);
            if now().duration_since(since) >= ENCODER_RECOVERY {
                anyhow::bail!("the encoder stopped returning frames");
            }
            warnings.set("encoder_recovery", format!("The encoder returned no frame for {} ms; recreating the same encoder while the picture freezes. Lower game GPU load or update the graphics driver if this repeats.", limit.as_millis()));
            tracing::warn!(
                wait_ms = limit.as_millis(),
                recreation = self.recreations + 1,
                recovery_ms = now().duration_since(since).as_millis(),
                "encoder stall recovery"
            );
            encoder.as_ref().unwrap().log_stall();
            self.recreations += 1;
            *encoder = None;
            return Ok(None);
        }
        let output = self.collect(encoder, warnings, now)?;
        // Output is progress even when the in-flight count did not drop.
        if !output.is_empty() {
            self.backlog_since = None;
        }
        Ok(Some(output))
    }
}
/// Backoff only after reopening fails; resource release is acknowledged.
const RECOVERY_RETRY: Duration = Duration::from_millis(150);
/// How long capture waits for the stream's display to come back before it
/// shows another one. Switching a virtual display back on takes about a
/// second; recreating it takes up to the driver's 10 s arrival deadline.
const DISPLAY_RETURN_WAIT: Duration = Duration::from_secs(15);
/// Which display a capture reopen uses.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ReopenOn {
    /// The stream's display.
    Stream,
    /// Not yet: the stream's display is not on the desktop.
    Wait,
    /// The primary display, after the stream's did not return in time.
    Primary,
}
/// Capture follows the stream's display. Windows can switch a virtual display
/// off (a game resetting the layout on exit); the heartbeat switches it back
/// on or recreates it and publishes it, possibly under a new name. Capture
/// waits for that and never quietly shows another desktop: only after
/// `DISPLAY_RETURN_WAIT` does it open the primary display, with a warning.
fn reopen_on(present: bool, missing_since: &mut Option<Instant>, now: Instant) -> ReopenOn {
    if present {
        *missing_since = None;
        return ReopenOn::Stream;
    }
    let since = *missing_since.get_or_insert(now);
    if now.duration_since(since) >= DISPLAY_RETURN_WAIT {
        ReopenOn::Primary
    } else {
        ReopenOn::Wait
    }
}
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
    warnings: &butterpollo_core::session::Warnings,
) -> Option<butterpollo_windows::truehdr::Filter> {
    if image.pixel != butterpollo_windows::capture::Pixel::Bgra8 {
        return None;
    }
    match butterpollo_windows::truehdr::Filter::new_gpu(image, rtx_parameters(config)) {
        Ok(filter) => {
            warnings.clear("display_truehdr");
            Some(filter)
        }
        Err(error) => {
            warnings.set("display_truehdr", format!("TrueHDR unavailable ({error:#}); using neutral SDR-to-PQ conversion without HDR enhancement. Check TrueHDR support or disable TrueHDR for this app."));
            None
        }
    }
}

pub(crate) fn effective_config(h: &Shared, launch: &Launch) -> Result<Config> {
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
fn finish_worker(worker: thread::JoinHandle<()>, name: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !worker.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if worker.is_finished() {
        let _ = worker.join();
    } else {
        tracing::error!(
            worker = name,
            "teardown did not return within five seconds; detaching the driver worker"
        );
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.thread.take() {
            finish_worker(worker, "capture");
        }
    }
}
impl std::ops::Deref for Source {
    type Target = Latest;
    fn deref(&self) -> &Latest {
        &self.latest
    }
}
type Route = butterpollo_windows::audio_route::Route;
/// Keep the streaming speakers the Windows default and find the device to
/// capture; the previous sink when Windows cannot say.
fn route_upkeep(
    route: &Route,
    config: &Config,
    warnings: &butterpollo_core::session::Warnings,
    previous: &str,
) -> String {
    match route.maintain_default() {
        Ok(()) => warnings.clear("audio_default"),
        Err(error) => warnings.set("audio_default", format!("Could not keep the streaming playback device as default ({error:#}); game audio may go to another device. Check the Windows default playback device.")),
    }
    match route.capture_sink(config) {
        Ok(selected) => {
            warnings.clear("audio_device_query");
            selected
        }
        Err(error) => {
            warnings.set("audio_device_query", format!("Could not find the current audio capture device ({error:#}); retaining the previous route. Check the Windows default playback device if sound is missing."));
            previous.to_owned()
        }
    }
}
/// The audio route's once-a-second upkeep, on its own thread. Each round
/// asks the Windows audio service for the default devices, several COM calls;
/// on the audio sender they held up the read after them about once a second.
struct RouteUpkeep {
    sink: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl RouteUpkeep {
    /// The first round runs on the caller, so capture opens on the right sink.
    fn start(
        route: Arc<Route>,
        config: Config,
        warnings: Arc<butterpollo_core::session::Warnings>,
    ) -> Result<Self> {
        let sink = Arc::new(Mutex::new(route_upkeep(
            &route,
            &config,
            &warnings,
            &route.sink,
        )));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new().name("audio route".into()).spawn({
            let sink = sink.clone();
            let stop = stop.clone();
            move || {
                let _com = match ComGuard::new() {
                    Ok(com) => com,
                    Err(error) => {
                        tracing::warn!(%error, "audio route upkeep unavailable");
                        return;
                    }
                };
                loop {
                    thread::park_timeout(Duration::from_secs(1));
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let previous = sink.lock().unwrap().clone();
                    let selected = route_upkeep(&route, &config, &warnings, &previous);
                    *sink.lock().unwrap() = selected;
                }
            }
        })?;
        Ok(Self {
            sink,
            stop,
            thread: Some(thread),
        })
    }
    fn sink(&self) -> String {
        self.sink.lock().unwrap().clone()
    }
}
impl Drop for RouteUpkeep {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.thread.take() {
            worker.thread().unpark();
            finish_worker(worker, "audio route");
        }
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
                    // The capture, whether it shows the primary display in
                    // place of the stream's, which is gone, and the stream's
                    // display it was opened for. The display can be recreated
                    // while capture opens; the next check then sees that.
                    let reopen = |lost: Capture, target: &(String, u64)| -> Result<(Capture, bool, (String, u64))> {
                        let recovery_started = Instant::now();
                        // Desktop Duplication reports the pointer's shape only
                        // when it changes; the new capture starts from this one.
                        let mut pointer = lost.pointer();
                        worker.begin_recovery()?;
                        drop(lost);
                        let deadline = Instant::now() + Duration::from_secs(30);
                        while !worker.consumers_released() {
                            worker.check()?;
                            if worker_stop.load(Ordering::Acquire) {
                                anyhow::bail!("capture stopped while releasing resources");
                            }
                            anyhow::ensure!(Instant::now() < deadline, "streams did not release the lost capture device");
                            timer.until(Instant::now() + Duration::from_millis(2));
                        }
                        let release_ms = recovery_started.elapsed().as_millis();
                        thread::sleep(RECOVERY_RETRY);
                        let mut missing_since = None;
                        loop {
                            worker.check()?;
                            anyhow::ensure!(Instant::now() < deadline, "capture did not recover within 30 seconds");
                            if worker_stop.load(Ordering::Acquire) {
                                anyhow::bail!("capture stopped while recovering");
                            }
                            let next = prepared.capture_target();
                            // A UAC prompt or the lock screen switches the input
                            // desktop; duplication must be made on that desktop.
                            butterpollo_windows::input::follow_input_desktop();
                            let on = reopen_on(butterpollo_windows::capture::display_present(&next.0), &mut missing_since, Instant::now());
                            let opened = match on {
                                ReopenOn::Wait => {
                                    thread::sleep(RECOVERY_RETRY);
                                    continue;
                                }
                                ReopenOn::Stream => Capture::open_for_stream_reported(&next.0, &kind, hdr, &capture_config, capture_warnings.clone()),
                                // By name: the WGC helper checks it opened the display asked for.
                                ReopenOn::Primary => butterpollo_windows::capture::primary_display()
                                    .context("no display is on the desktop")
                                    .and_then(|primary| Capture::open_for_stream_reported(&primary, &kind, hdr, &capture_config, capture_warnings.clone())),
                            };
                            match opened {
                                Ok(mut recovered) => {
                                    if on == ReopenOn::Primary {
                                        capture_warnings.set("capture_display", format!("The stream's display {} disappeared and did not return within {} s; showing the primary display until it returns. A game or Windows changed the display layout; reconnect if the picture does not return.", next.0, DISPLAY_RETURN_WAIT.as_secs()));
                                    } else {
                                        capture_warnings.clear("capture_display");
                                    }
                                    if next != *target {
                                        tracing::info!(output = %next.0, "capture moved to the recreated display");
                                    }
                                    if let Some(pointer) = pointer.take()
                                        && let Err(error) = recovered.resume_pointer(pointer)
                                    {
                                        tracing::warn!(%error, "the pointer appears once it moves or changes");
                                    }
                                    tracing::info!(release_ms, elapsed_ms=recovery_started.elapsed().as_millis(), backend=recovered.backend(), "capture reopened after resource release");
                                    return Ok((recovered, on == ReopenOn::Primary, next));
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
                    let mut on_primary = false;
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
                            // Back from the primary display once the stream's returns.
                            let returned = on_primary && butterpollo_windows::capture::display_present(&next.0);
                            if worker.take_device_restart()? || next != target || return_to_wgc || returned {
                                let lost = std::mem::replace(&mut capture, Capture::Closed);
                                (capture, on_primary, target) = match reopen(lost, &next) {
                                    Ok(capture) => capture,
                                    Err(_) if worker_stop.load(Ordering::Acquire) => return Ok(()),
                                    Err(error) => return Err(error),
                                };
                                capture.set_claim_grid(worker_grid.clone(), aligned);
                            }
                        }
                        // Reset before the helper's announcements are read: one
                        // that comes after them ends the wait below.
                        if let Some(signal) = capture.frame_signal() {
                            signal.reset()?;
                        }
                        let captured = (|| {
                            #[cfg(debug_assertions)]
                            crate::soak_fault::check("DXGI_ERROR_ACCESS_LOST")?;
                            capture.next_gpu()
                        })();
                        match captured {
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
                                if let Some(loss) = DeviceLost::from_error(&e) {
                                    worker.device_lost(loss, &capture_warnings, Instant::now())?;
                                    worker.take_device_restart()?;
                                } else {
                                    capture_warnings.set("capture_recovery", format!("Capture interrupted ({e:#}); reopening capture, with a frozen picture until frames resume. If this repeats, keep the display mode stable and check the WGC helper and graphics driver."));
                                }
                                let lost = std::mem::replace(&mut capture, Capture::Closed);
                                (capture, on_primary, target) = match reopen(lost, &target) {
                                    Ok(capture) => capture,
                                    Err(_) if worker_stop.load(Ordering::Acquire) => return Ok(()),
                                    Err(error) => return Err(error),
                                };
                                capture.set_claim_grid(worker_grid.clone(), aligned);
                            }
                        }
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    if DeviceLost::from_error(&e).is_some() {
                        capture_warnings.set("gpu_recovery", format!("{e:#}"));
                    }
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
        let result = self.spawn_session(&h, &s);
        if let Err(e) = result {
            h.sessions.lock().unwrap().fail_start(&s);
            tracing::error!(error=%e,"could not start session worker");
        }
    }
    fn open_session_capture<'a>(
        &self,
        s: &Arc<Session>,
        c: &Config,
        prepared: &Arc<crate::display_session::Ready>,
        latest: &'a mut Option<Arc<Source>>,
    ) -> Result<(&'a mut Arc<Source>, Arc<capture::Consumer>)> {
        let latest = latest.insert(self.capture(
            &prepared.capture(),
            s.config.hdr,
            c,
            butterpollo_core::framegen::Rate(s.config.fps_millihz()),
            &s.launch.id,
            prepared.clone(),
        )?);
        *s.capture_warnings.write().unwrap() = latest.warnings.clone();
        let capture_wake = latest.subscribe(Arc::downgrade(s))?;
        capture_wake.wake_on_recovery(s);
        Ok((latest, capture_wake))
    }
    fn configure_encoder(
        s: &Session,
        c: &Config,
        first: &GpuImage,
        prepared: &crate::display_session::Ready,
        encoder: &mut Option<Encoder>,
    ) -> Result<Option<&'static str>> {
        *encoder = Some(Encoder::new_gpu_reported(
            &s.config,
            c.get("encoder", "auto"),
            first,
            c,
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
            .ok()
            .map(|mode| mode.dmDisplayFrequency);
        tracing::info!(width=s.config.width,height=s.config.height,fps=f64::from(s.config.fps_millihz())/1000.,codec=s.config.codec,hdr=s.config.hdr,full_range=s.config.full_range(),color_matrix=s.config.color_matrix(),vrr=s.config.vrr_low_latency,requested_capture=%prepared.capture(),requested_encoder=c.get("encoder","auto"),encoder=encoder.as_ref().unwrap().backend(),adapter=%first.gpu.display.adapter,source_width=first.width,source_height=first.height,source_refresh_hz,source_pixel=?first.pixel,"stream configured");
        let minimum = butterpollo_core::pyrowave::minimum_kbps(
            s.config.width,
            s.config.height,
            s.config.fps_millihz(),
        );
        let recommended = butterpollo_core::pyrowave::recommended_kbps(
            s.config.width,
            s.config.height,
            s.config.fps_millihz(),
        );
        let bitrate = s.bitrate.load(Ordering::Relaxed);
        if s.config.codec == 3 && bitrate < minimum {
            tracing::warn!(
                bitrate_kbps = bitrate,
                minimum_kbps = minimum,
                recommended_kbps = recommended,
                "PyroWave bitrate is too low: severe detail loss is likely. Raise the bitrate in Moonlight with network headroom, or use HEVC or AV1"
            );
        } else if s.config.codec == 3 && bitrate < recommended {
            tracing::warn!(
                bitrate_kbps = bitrate,
                minimum_kbps = minimum,
                recommended_kbps = recommended,
                "PyroWave bitrate is below recommended: text and textures may lose detail. Quality depends on the picture; raise the bitrate in Moonlight with network headroom, or use HEVC or AV1"
            );
        }
        let metadata = first.gpu.hdr_metadata();
        *s.hdr_metadata.write().unwrap() = metadata;
        if let Some(encoder) = encoder.as_mut() {
            encoder.set_hdr_metadata(metadata);
        }
        Ok(pinned_backend)
    }
    fn spawn_audio(
        self: &Arc<Self>,
        h: &Shared,
        s: &Arc<Session>,
    ) -> std::io::Result<thread::JoinHandle<()>> {
        let audio_m = self.clone();
        let audio_h = h.clone();
        let audio_s = s.clone();
        thread::Builder::new().name("audio".into()).spawn(move || {
            if let Err(e) = audio_m.audio(audio_h, audio_s.clone()) {
                audio_s.launch.warnings.set("audio_stopped", format!("Audio stopped ({e:#}); video is still running without sound. Check the playback device and network, then reconnect."));
            }
        })
    }
    fn spawn_session(
        self: &Arc<Self>,
        h: &Shared,
        s: &Arc<Session>,
    ) -> std::io::Result<thread::JoinHandle<()>> {
        let m = self.clone();
        let worker_h = h.clone();
        let worker_s = s.clone();
        thread::Builder::new()
            .name("session".into())
            .spawn(move || {
                let h = worker_h;
                let s = worker_s;
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
                let mut com = None;
                let mut stream_preparation = None;
                let mut latest = None;
                let mut encoder = None;
                let mut client_commands = None;
                let mut audio = None;
                let mut pyrowave_sender = None;
                #[cfg(test)]
                let mut fixture_preparation = None;
                let result = (|| -> Result<()> {
                    #[cfg(test)]
                    if let Some(fixture) = &h.reconnect_fixture {
                        fixture_preparation = s.launch.preparation.lock().unwrap().take();
                        return fixture.stream(&m, &h, &s);
                    }
                    com = Some(ComGuard::new()?);
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
                        let monitors = butterpollo_windows::input::on_input_desktop(
                            butterpollo_windows::display::monitors,
                        )?;
                        let monitor = monitors
                            .iter()
                            .find(|m| m.matches(output))
                            .or_else(|| monitors.iter().find(|m| m.primary))
                            .context("input display unavailable")?;
                        *s.output.write().unwrap() = monitor.display_name.clone();
                        client_commands = Some(crate::process::ClientCommands::start(&h, &s)?);
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
                        .take();
                    let stream_preparation = stream_preparation.insert(match initial {
                        Some(p) if p.prepared().display.matches(&s.config) => p.into_prepared(),
                        previous => {
                            drop(previous);
                            h.app_display.lock().unwrap().remove(&s.launch.client.uuid);
                            crate::display_session::prepare_stream(
                                &h, &s.launch, &s.config, &c,
                            )?
                        }
                    });
                    stream_preparation.report_limiter(&s.launch.warnings, &c);
                    let prepared = stream_preparation.display.clone();
                    if s.launch.role == Role::Stream {
                        h.app_display
                            .lock()
                            .unwrap()
                            .insert(s.launch.client.uuid.clone(), (prepared.clone(), None));
                    }
                    let output = prepared.output();
                    *s.output.write().unwrap() = output.clone();
                    let mut use_truehdr = s.config.hdr && rtx_enabled(&c);
                    let (latest, mut capture_wake) = m.open_session_capture(&s, &c, &prepared, &mut latest)?;
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
                    let pinned_backend = Self::configure_encoder(&s, &c, &first, &prepared, &mut encoder)?;
                    let mut truehdr = if use_truehdr {
                        truehdr_filter(&first, &c, &s.launch.warnings)
                    } else {
                        None
                    };
                    drop(first);
                    let mut truehdr_staging = None;
                    client_commands = Some(crate::process::ClientCommands::start(&h, &s)?);
                    audio = Some(m.spawn_audio(&h, &s)?);
                    let requested_fec = c.integer("fec_percentage", 20);
                    if requested_fec != requested_fec.clamp(0, 100) {
                        s.launch.warnings.set("network_fec_config", format!("FEC percentage {requested_fec} is outside 0-100; using {}%. Correct fec_percentage in Network settings.", requested_fec.clamp(0, 100)));
                    }
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
                    pyrowave_sender = if s.config.codec == 3 { Some(crate::pyrowave_send::Sender::new(m.video.clone(),s.clone(),c.clone(),h.clone(),start,prepared.capture() == "wgc")?) } else { None };
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
                    let mut recovery = EncoderRecovery::default();
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
                    // As in Vibepollo, an unset minimum is 20 for every codec,
                    // PyroWave too: its old default of the full stream rate
                    // repeated a still picture every period, and under VRR a
                    // game frame a little late followed a repeat.
                    let minimum_fps = c
                        .get("minimum_fps_target", "20")
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
                    let mut video_qos = Tagged::default();
                    let mut batch = butterpollo_windows::net::Batch::default();
                    let trace_send = tracing::enabled!(target: "pacing", tracing::Level::TRACE);
                    batch.waits = trace_send.then(Default::default);
                    let mut network_pacer = butterpollo_core::network_pacing::Pacer::new(Instant::now());
                    let mut link = None;
                    let mut link_due = Instant::now();
                    let mut reported_pacing = None;
                    let mut fec_reported = None;
                    let send_outage = butterpollo_core::stream_policy::SendOutage::from_env();
                    // On a link paced near the stream bitrate, sending a frame
                    // takes a good part of a period; a sender thread lets the
                    // next picture be claimed meanwhile. On a fast link the
                    // inline send is shorter than the handoff costs.
                    let video_sender = std::cell::OnceCell::<crate::video_send::Sender>::new();
                    let mut sender_decided = false;
                    let mut send_loss = butterpollo_core::stream_policy::SendLossRecovery::default();
                    let batch_kb = match c.integer("video_max_batch_size_kb", 64) {
                        16 => 16,
                        32 => 32,
                        _ => 64,
                    };
                    let mut send_frames = |output: Vec<butterpollo_windows::encoder::Encoded>,
                                           peer: std::net::SocketAddr,
                                           call_latency: Duration,
                                           source: &Source|
                     -> Result<()> {
                        if !output.is_empty() {
                            s.launch.warnings.clear("encoder_recovery");
                            source.device_recovered(&source.warnings);
                        }
                        if let Some(sender) = &pyrowave_sender { return sender.submit(output,peer,call_latency); }
                        if !sender_decided {
                            sender_decided = true;
                            let route = *link.get_or_insert_with(|| butterpollo_windows::net::routed_link(peer));
                            let bps = butterpollo_core::network_pacing::rate_bps(
                                c.integer("pacing_max_bitrate_kbps", 0),
                                s.bitrate.load(Ordering::Relaxed),
                                route.bps,
                                route.wireless,
                            );
                            // Only before the first frame: the sender numbers
                            // frames and packets from the start.
                            if packetizer.frame == 1
                                && c.boolean("video_send_thread", true)
                                && butterpollo_core::network_pacing::paced(bps, s.bitrate.load(Ordering::Relaxed))
                            {
                                let _ = video_sender.set(crate::video_send::Sender::new(m.video.clone(), s.clone(), c.clone(), h.clone(), start, prepared.capture() == "wgc")?);
                                tracing::info!(pacing_bps = bps, bitrate_kbps = s.bitrate.load(Ordering::Relaxed), "video frames are sent on their own thread: pacing is near the stream bitrate");
                            }
                        }
                        if let Some(sender) = video_sender.get() { return sender.submit(output, peer, call_latency); }
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
                                    s.launch.warnings.event("network_frame", format!("Encoded video frame dropped ({error:#}); requesting a recovery frame. Lower bitrate or resolution to stay within Moonlight's packet limit."), butterpollo_core::session::EVENT_PERIOD);
                                    s.request_idr();
                                    continue;
                                }
                            };
                            // A keyframe, as at the start of every stream, may be too
                            // large for FEC; only ordinary frames make this worth showing.
                            if !frame.idr
                                && packetizer.fec_limited(frame.bytes.len())
                                && fec_reported.is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs(1))
                            {
                                fec_reported = Some(Instant::now());
                                s.launch.warnings.event("network_fec", "FEC was reduced or omitted for large video frames because they exceed Moonlight's four-block limit. Packet loss in those frames is harder to recover; lower bitrate or resolution to keep full FEC protection.", butterpollo_core::session::EVENT_PERIOD);
                            }
                            next_wire_frame.set(u64::from(packetizer.frame));
                            let frame_bytes = packets.iter().map(|p|p.len() as u64).sum();
                            let route = *link.get_or_insert_with(|| butterpollo_windows::net::routed_link(peer));
                            let bps = butterpollo_core::network_pacing::rate_bps(
                                c.integer("pacing_max_bitrate_kbps", 0),
                                s.bitrate.load(Ordering::Relaxed),
                                route.bps,
                                route.wireless,
                            );
                            if Instant::now() >= link_due {
                                let needed = u64::from(s.bitrate.load(Ordering::Relaxed)) * (100 + packetizer.fec_percent as u64) * 10;
                                butterpollo_core::network_pacing::report_rate(&s.launch.warnings, bps, needed, c.integer("pacing_max_bitrate_kbps", 0));
                                if reported_pacing != Some(bps) {
                                    tracing::info!(pacing_bps=bps, link_bps=route.bps, configured_kbps=c.integer("pacing_max_bitrate_kbps", 0), "network pacing selected; defaults use twice encoder bitrate for confirmed wireless routes, or the wired fallback ceiling");
                                    reported_pacing = Some(bps);
                                }
                            }
                            let dropped = batch.dropped;
                            let waits_before = batch.waits;
                            let mut pacing_wait = Duration::ZERO;
                            let mut send_time = Duration::ZERO;
                            let mut batches = 0;
                            let mut first_send = None;
                            let mut last_send = None;
                            let mut remaining = packets.as_slice();
                            let mut lost = butterpollo_core::stream_policy::SendLoss::new(packetizer.block_layout(frame.bytes.len()));
                            let mut abandoned = 0;
                            s.stats.video_frame.store(u64::from(packetizer.frame.wrapping_sub(1)) + 1, Ordering::Release);
                            while !remaining.is_empty() {
                                if s.stopping() || h.stop.load(Ordering::Acquire) {
                                    return Ok(());
                                }
                                let now = Instant::now();
                                if network_pacer.due() > now {
                                    timer.until_precise(network_pacer.due());
                                    if trace_send { pacing_wait += now.elapsed(); }
                                }
                                let budget = (bps / 4000)
                                    .clamp(remaining[0].len() as u64, batch_kb * 1024)
                                    as usize;
                                let count =
                                    butterpollo_windows::net::Batch::count(remaining, budget);
                                let send_started = trace_send.then(Instant::now);
                                let refused = batch.dropped;
                                let bytes = if send_outage.is_some_and(|outage| outage.active(start, Instant::now())) {
                                    batch.refuse(count);
                                    0
                                } else {
                                    batch.send(&m.video, &remaining[..count], peer)?
                                };
                                if let Some(send_started) = send_started {
                                    let finished = Instant::now();
                                    first_send.get_or_insert(send_started);
                                    last_send = Some(finished);
                                    send_time += finished.duration_since(send_started);
                                    batches += 1;
                                }
                                remaining = &remaining[count..];
                                network_pacer.sent(Instant::now(), bytes, if bytes > 0 { count } else { 0 }, peer.ip().to_canonical().is_ipv6(), bps);
                                s.stats.packets.fetch_add(count as u64, Ordering::Relaxed);
                                s.stats.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
                                // The socket refused more of this frame than FEC
                                // repairs: the client cannot decode it. Stop
                                // spending the link on it, and make the next
                                // frame the keyframe the client will wait for.
                                if lost.sent(count, (batch.dropped - refused) as usize) {
                                    abandoned = remaining.len();
                                    if send_loss.lost(Instant::now(), period, lost.reached_socket()) {
                                        s.request_send_loss_recovery();
                                    }
                                    break;
                                }
                            }
                            if lost.unrecoverable() {
                                tracing::debug!(frame = packetizer.frame.wrapping_sub(1), idr = frame.idr, abandoned, "video frame lost on send; the rest of it was not sent");
                            } else {
                                send_loss.delivered(frame.idr);
                            }
                            if batch.dropped != dropped {
                                s.launch.warnings.event("network_send", "Video packets were dropped by the host after transient socket send failures. You may see stutter or recovery frames; lower bitrate and check the network adapter. The log includes the socket error code.", butterpollo_core::session::EVENT_PERIOD);
                            }
                            s.stats.frames.fetch_add(1, Ordering::Relaxed);
                            let sent = Instant::now();
                            if let (Some(first), Some(last), Some(before), Some(after)) = (first_send, last_send, waits_before, batch.waits) {
                                tracing::trace!(target: "pacing", frame=packetizer.frame.wrapping_sub(1), packets=packets.len(), batches,
                                    pacing_wait_us=micros(pacing_wait), send_us=micros(send_time),
                                    first_send_us=micros(first.saturating_duration_since(start)), last_send_us=micros(last.saturating_duration_since(start)),
                                    writable_waits=after.count-before.count, writable_wait_us=micros(after.elapsed-before.elapsed),
                                    dropped=batch.dropped-dropped, stream_id=%s.launch.id, "send");
                            }
                            s.stats.performance.lock().unwrap().record_timing(sent,butterpollo_core::performance::Timing{period,encode:latency,host:processing,age,sent:micros(sent.saturating_duration_since(claimed))},frame_bytes);
                            // The interface lookup takes a moment: refresh the
                            // link speed after the frame is out, for the next one.
                            if Instant::now() >= link_due {
                                link = Some(butterpollo_windows::net::routed_link(peer));
                                link_due = Instant::now() + Duration::from_secs(2);
                            }
                        }
                        Ok(())
                    };
                    (|| -> Result<()> {
                        while !s.stopping() && !h.stop.load(Ordering::Acquire) {
                            if let Some(sender) = video_sender.get() { sender.backlog()?; }
                            if let Some(loss) = recovery.device_loss.take() {
                                latest.latest.device_lost(loss, &latest.warnings, Instant::now())?;
                                encoder = None;
                            }
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
                                    *latest = m.capture(
                                        &prepared.capture(),
                                        s.config.hdr,
                                        runtime,
                                        butterpollo_core::framegen::Rate(s.config.fps_millihz()),
                                        &s.launch.id,
                                        prepared.clone(),
                                    )?;
                                    *s.capture_warnings.write().unwrap() = latest.warnings.clone();
                                    capture_wake = latest.subscribe(Arc::downgrade(&s))?;
                                    capture_wake.wake_on_recovery(&s);
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
                                        send_loss_recoveries=s.stats.send_loss_recoveries.load(Ordering::Relaxed),
                                        fec_reports=timing["fec_reports"].as_u64().unwrap_or(0),
                                        fec_invalid_reports=timing["fec_invalid_reports"].as_u64().unwrap_or(0),
                                        fec_duplicate_reports=timing["fec_duplicate_reports"].as_u64().unwrap_or(0),
                                        fec_recovered_frames=timing["fec_recovered_frames"].as_u64().unwrap_or(0),
                                        fec_unrecoverable_frames=timing["fec_unrecoverable_frames"].as_u64().unwrap_or(0),
                                        fec_missing_packets=timing["fec_missing_packets"].as_u64().unwrap_or(0),
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
                                    if encoder.as_ref().is_some_and(|e| recovery.pending(e)) {
                                        send_frames(recovery.collect(&mut encoder, &s.launch.warnings, Instant::now)?, peer, Duration::ZERO, latest)?;
                                        if encoder.as_ref().is_some_and(|e| recovery.pending(e)) {
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
                                if encoder.as_ref().is_some_and(|e| recovery.pending(e))
                                    && deadline.saturating_duration_since(Instant::now()) >= Duration::from_millis(1)
                                {
                                    send_frames(recovery.collect(&mut encoder, &s.launch.warnings, Instant::now)?, peer, Duration::ZERO, latest)?;
                                }
                                let until = if encoder.as_ref().is_some_and(|e| recovery.pending(e)) {
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
                            // A recovery request on a moving picture rides the
                            // next new frame; only a still one is encoded again
                            // at once. Grid pacing serves it at the next slot.
                            let recovering = s.idr.load(Ordering::Acquire) || s.invalidation.lock().unwrap().is_some();
                            let reencode_at = if !recovering {
                                repeat_due
                            } else if arrival_pacing {
                                butterpollo_core::stream_policy::reencode_at(true, image.captured, period, latest.source_interval(), repeat_due)
                            } else {
                                Instant::now()
                            };
                            if !rebuild_encoder
                                && (s.config.vrr_low_latency || limit_static_rate || arrival_pacing)
                                && last_image
                                    .as_ref()
                                    .is_some_and(|previous| Arc::ptr_eq(previous, &image))
                                && Instant::now() < reencode_at
                            {
                                if encoder.as_ref().is_some_and(|e| recovery.pending(e)) { send_frames(recovery.collect(&mut encoder, &s.launch.warnings, Instant::now)?, peer, Duration::ZERO, latest)?; }
                                let wait = if encoder.as_ref().is_some_and(|e| recovery.pending(e)) { OUTPUT_POLL } else { period };
                                latest.wait_if_current(&timer, &capture_wake, &image, Instant::now() + wait.min(reencode_at.saturating_duration_since(Instant::now())))?;
                                continue;
                            }
                            // A picture claimed while the encoder is behind only
                            // waits in its queue: take its output first, then
                            // claim the newest picture.
                            // A frame still being sent on a paced link counts too.
                            // A frame waiting behind it is skipped on a keyframe
                            // request: the client would discard it, and the
                            // keyframe would wait for it.
                            let sending = match video_sender.get() {
                                Some(sender) if s.idr.load(Ordering::Acquire) => sender.skip_pending()?,
                                Some(sender) => sender.backlog()?,
                                None => 0,
                            };
                            let encoding = encoder.as_ref().map_or(0, |e| recovery.backlog(e));
                            if !rebuild_encoder && encoding + sending >= ENCODER_BACKLOG {
                                if encoding < ENCODER_BACKLOG {
                                    // Network occupancy must not start encoder stall recovery.
                                    recovery.backlog_since = None;
                                    if encoder.as_ref().is_some_and(|e| recovery.pending(e)) {
                                        send_frames(recovery.collect(&mut encoder, &s.launch.warnings, Instant::now)?, peer, Duration::ZERO, latest)?;
                                    } else {
                                        timer.until(Instant::now() + OUTPUT_POLL);
                                    }
                                    continue;
                                }
                                // An encoder that returns nothing for 250 ms is
                                // recreated, as a queue that never drained was;
                                // a shorter stall on a saturated GPU only makes the
                                // stream choppy. A recreated one that is still
                                // silent gets longer each time.
                                if let Some(output) = recovery.poll_full(&mut encoder, &s.launch.warnings, Instant::now)? {
                                    send_frames(output, peer, Duration::ZERO, latest)?;
                                }
                                continue;
                            }
                            recovery.backlog_since = None;
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
                                if let Some(since) = recovery.failing
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
                                        if let Some(loss) = DeviceLost::d3d11(&image.gpu.device).or_else(|| DeviceLost::from_error(&error)) {
                                            recovery.device_loss = Some(loss);
                                            recovery.failing.get_or_insert_with(Instant::now);
                                            continue;
                                        }
                                        let since = *recovery.failing.get_or_insert_with(Instant::now);
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
                                    truehdr_filter(&image, &runtime_config, &s.launch.warnings)
                                } else {
                                    None
                                };
                                truehdr_staging = None;
                                recovery.recreated(&s);
                                rebuild_encoder = false;
                            }
                            let active = encoder
                                .as_mut()
                                .expect("a missing encoder is rebuilt above");
                            if rebuilt {
                                active.set_next_frame(match video_sender.get() {
                                    Some(sender) => sender.next_wire_frame()?,
                                    None => next_wire_frame.get(),
                                });
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
                            let invalidation = s.invalidation.lock().unwrap().take();
                            if let Some((first, last)) = invalidation
                                && !active.invalidate_ref_frames(first, last)
                            {
                                s.request_idr();
                            }
                            let idr = s.idr.swap(false, Ordering::AcqRel);
                            let recovering = idr || invalidation.is_some();
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
                            active.set_repeat(!fresh);
                            let mut presented_image = image.as_ref().clone();
                            if !fresh { presented_image.captured = Instant::now(); }
                            let transformed = if converted { truehdr.as_mut().map(|filter| filter.apply_gpu(&presented_image)).transpose() } else { Ok(None) };
                            let encoded = (|| -> Result<Vec<butterpollo_windows::encoder::Encoded>> {
                                #[cfg(debug_assertions)]
                                crate::soak_fault::check("encoder failure")?;
                                #[cfg(debug_assertions)]
                                crate::soak_fault::check("DXGI_ERROR_DEVICE_REMOVED")?;
                                Ok(if let Ok(Some(transformed)) = transformed.as_ref() {
                                    active.encode_gpu(transformed, idr, bitrate)?
                                } else if let Err(error) = transformed {
                                    s.launch.warnings.set("display_truehdr", format!("TrueHDR conversion failed ({error:#}); continuing with SDR-to-PQ without HDR enhancement. Check TrueHDR support or disable it for this app."));
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
                                    recovery.output(output, Instant::now)?
                                }
                                Err(error) => {
                                    if let Some(loss) = active.device_removed()
                                        .or_else(|| DeviceLost::d3d11(&image.gpu.device))
                                        .or_else(|| DeviceLost::from_error(&error))
                                    {
                                        recovery.device_loss = Some(loss);
                                        recovery.failing.get_or_insert_with(Instant::now);
                                        continue;
                                    }
                                    // A stalled or reset GPU costs these frames and a
                                    // keyframe; the rebuild requests it. Only failures
                                    // that keep coming end the session.
                                    let since = *recovery.failing.get_or_insert_with(Instant::now);
                                    if since.elapsed() >= ENCODER_RECOVERY {
                                        return Err(error.context("the encoder kept failing"));
                                    }
                                    s.launch.warnings.set("encoder_recovery", format!("Encoding failed ({error:#}); recreating the same encoder while the picture freezes. Lower game GPU load or update the graphics driver if this repeats."));
                                    active.log_stall();
                                    rebuild_encoder = true;
                                    continue;
                                }
                            };
                            let call_latency = begin.elapsed();
                            // Repeat deadlines start at submission, so encoder work
                            // does not extend the interval between static frames.
                            encoded_at = begin;
                            if arrival_pacing {
                                // New pictures and static repeats count toward the
                                // stream rate: a repeat that spent nothing let the
                                // game's next frame follow it at once, so VRR
                                // streams went past the rate whenever a frame came
                                // late. An unchanged picture encoded again for the
                                // client's recovery request does not, or a client
                                // losing packets left every game frame late.
                                if butterpollo_core::stream_policy::counts_toward_rate(fresh, recovering) {
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
                            send_frames(output, peer, call_latency, latest)?;
                        }
                        Ok(())
                    })()
                })();
                if let Err(e) = result {
                    s.launch.warnings.set("stream_failure", format!("Stream ended: {e:#}"));
                    s.fail();
                    tracing::error!(error=%format!("{e:#}"),client=%s.launch.client.name,"session failed; check the reported encoder, capture or socket error before reconnecting");
                }
                s.stop();
                h.sessions.lock().unwrap().begin_teardown(&s);
                m.peers
                    .lock()
                    .unwrap()
                    .retain(|(id, _), _| id != &s.launch.id);
                // Keep capture and display leases owned until the encoder has
                // terminated, even if a driver blocks its drop indefinitely.
                if let Some(audio) = audio {
                    finish_worker(audio, "audio");
                }
                drop(pyrowave_sender);
                drop(client_commands);
                drop(encoder);
                drop(latest);
                drop(stream_preparation);
                #[cfg(test)]
                drop(fixture_preparation);
                drop(com);
                if s.launch.role == Role::RemoteMonitor
                    && h.config
                        .read()
                        .unwrap()
                        .boolean("remote_monitor_disconnect_on_stream_end", false)
                {
                    crate::remote_display::disconnect(&h, Some(&s.launch.client.uuid));
                }
                h.sessions.lock().unwrap().teardown.remove(&s.launch.id);
                tracing::info!(client=%s.launch.client.name,"CLIENT DISCONNECTED");
            })
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
            .map(|route| route.route());
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
        let mut last_loss = None;
        let mut upkeep: Option<RouteUpkeep> = None;
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
            #[cfg(debug_assertions)]
            if let Some(stall) = crate::soak_fault::audio_stall()? {
                thread::sleep(stall);
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
                                s.launch.warnings.set("audio_route", format!("Audio routing failed ({detail}); sending silence while retrying. Select an available playback or virtual audio device in Audio settings."));
                                audio_error = Some(detail);
                            }
                        }
                    }
                }
                if let Some(route) = &route {
                    s.launch.warnings.clear("audio_route");
                    if let Some(message) = route.warning(s.launch.host_audio) {
                        s.launch.warnings.set("audio_virtual_sink", message);
                    } else {
                        s.launch.warnings.clear("audio_virtual_sink");
                    }
                    let selected = match &upkeep {
                        Some(upkeep) => upkeep.sink(),
                        None => upkeep
                            .insert(RouteUpkeep::start(
                                route.clone(),
                                config.clone(),
                                s.launch.warnings.clone(),
                            )?)
                            .sink(),
                    };
                    if selected != sink {
                        if !sink.is_empty() {
                            s.launch.warnings.event("audio_device_changed", "Audio capture device changed because the Windows default playback device changed. WASAPI is reopening and sound may briefly pause; select a fixed capture sink if the change was unintended.", butterpollo_core::session::EVENT_PERIOD);
                        }
                        capture = None;
                        sink = selected;
                        capture_failed = false;
                    }
                    if capture.is_none()
                        && (!capture_failed || config.boolean("auto_capture_sink", true))
                    {
                        match route.set_channels(s.config.audio_channels as usize) {
                            Ok(()) => s.launch.warnings.clear("audio_surround"),
                            Err(error) => s.launch.warnings.set("audio_surround", format!("Virtual surround format could not be applied ({error:#}); channels may be downmixed. Select stereo or check the virtual audio driver.")),
                        }
                        match Loopback::new_sink(s.config.audio_channels as usize, &sink) {
                            Ok(value) => {
                                tracing::info!(
                                    channels = s.config.audio_channels,
                                    event_driven = value.event_driven(),
                                    "WASAPI audio capture started"
                                );
                                capture = Some(value);
                                s.launch.warnings.clear("audio_capture");
                                capture_failed = false;
                                audio_error = None;
                            }
                            Err(error) => {
                                capture_failed = true;
                                let detail = format!("{error:#}");
                                if audio_error.as_ref() != Some(&detail) {
                                    s.launch.warnings.set("audio_capture", format!("Audio capture failed ({detail}); sending silence while retrying. Check that the playback device is available and reconnect if it does not recover."));
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
            let waited = capture.as_mut().and_then(Loopback::since_drained);
            while let Some(capturing) = capture.as_mut() {
                match capturing.read(frames) {
                    Ok(Some(samples)) => packets.push(samples),
                    Ok(None) => break,
                    Err(error) => {
                        s.launch.warnings.set("audio_capture", format!("Audio capture interrupted ({error:#}); reopening WASAPI and sending silence until it recovers. Check the playback device if this repeats."));
                        capture = None;
                        capture_failed = true;
                    }
                }
            }
            if let Some(capturing) = capture.as_mut() {
                let delivered = capturing.take_delivered();
                if let Some(waited) = waited {
                    let lost = loss.read(waited, delivered, capturing.buffer());
                    // A skip no late read explains is the endpoint falling
                    // quiet and starting again.
                    let skipped = delivered.unwrap_or_default();
                    if waited > capturing.buffer() || !skipped.is_zero() {
                        tracing::debug!(
                            waited_ms = waited.as_secs_f64() * 1000.,
                            skipped_ms = skipped.as_secs_f64() * 1000.,
                            delivered = delivered.is_some(),
                            lost,
                            "audio sender read late or Windows skipped audio"
                        );
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
            let now = Instant::now();
            if let Some(report) = loss.report(now) {
                last_loss = Some(now);
                s.launch.warnings.set("audio_loss", report.message());
            } else if last_loss.is_some_and(|at| now.duration_since(at) >= Duration::from_secs(10))
            {
                s.launch.warnings.clear("audio_loss");
                last_loss = None;
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
        // Active sessions without a control peer, since when.
        let mut unattended: HashMap<String, Instant> = HashMap::new();
        let input_timer = butterpollo_windows::timing::Timer::new()?;
        let mut feedback_at = Instant::now();
        while !h.stop.load(Ordering::Acquire) {
            let ping_timeout = crate::network::ping_timeout(&h.config.read().unwrap());
            #[cfg(test)]
            if let Some(fixture) = &h.reconnect_fixture {
                fixture.reset_peer(&mut host, &peers);
            }
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
                            .filter(|s| !s.stopping())
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
                            // ENet's own peer timeout (about 5 s) would end a
                            // stream on a Wi-Fi drop the session itself
                            // tolerates; let ping_timeout decide instead.
                            let timeout =
                                u32::try_from(ping_timeout.as_millis()).unwrap_or(u32::MAX);
                            peer.set_timeout(32, timeout, timeout.max(30_000));
                            peers.insert(
                                peer.id(),
                                ControlPeer {
                                    #[cfg(test)]
                                    _fixture_input: h.reconnect_fixture.as_ref().map(|f| f.input()),
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
                                    denied: 0,
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
                            0x3000 => {
                                if let Some((value, elevated)) = server_command(
                                    &h,
                                    &s.launch.client,
                                    &payload,
                                    &mut p.command_at,
                                ) && let Err(e) =
                                    butterpollo_windows::process::Process::shell_detached(
                                        &value,
                                        None,
                                        elevated,
                                        &Default::default(),
                                    )
                                {
                                    tracing::warn!(error=%e, "configured server command failed");
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
                            // This dispatch receives only client-to-host packets;
                            // 0x5502 in the other direction is controller feedback.
                            0x5502 => s.record_fec_status(&payload),
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
                                    // Say once per kind that the device may not
                                    // send it: dropped silently, a phone's
                                    // controller looked undetected.
                                    Ok(event)
                                        if !s.launch.client.allows(event.required_permission()) =>
                                    {
                                        let permission = event.required_permission();
                                        if p.denied & permission == 0 {
                                            p.denied |= permission;
                                            let (code, message) =
                                                event.denied_warning(&s.launch.client.name);
                                            s.launch.warnings.set(&code, message);
                                        }
                                    }
                                    Ok(event) => {
                                        if let input::Input::Arrival { capabilities, .. } = &event
                                            && let Some(hint) =
                                                butterpollo_core::input_policy::steam_input_hint(
                                                    &s.launch.client.name,
                                                    *capabilities,
                                                )
                                        {
                                            s.launch.warnings.set("input_steam_input", hint);
                                        }
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
                #[cfg(test)]
                if h.reconnect_fixture.is_some() {
                    p.injector_tried = true;
                }
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
                        match Injector::new_options_reported(
                            if output.is_empty() {
                                c.get("output_name", "")
                            } else {
                                &output
                            },
                            c.get("gamepad", "auto"),
                            &c,
                            s.launch.warnings.clone(),
                        ) {
                            Ok(mut i) => {
                                s.launch.warnings.clear("input_initialization");
                                i.set_stream_size(s.config.width, s.config.height);
                                p.injector = Some(i);
                            }
                            Err(e) => s.launch.warnings.set("input_initialization", format!("Input initialization failed ({e:#}); controls are unavailable. Check Input settings and the selected display, then reconnect.")),
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
                            s.launch.warnings.event("input_injection", format!("Input injection failed ({e:#}); keyboard, mouse, touch or pen actions may be missing. Unlock the desktop and check Windows input permissions or disable native touch/pen if unsupported."), butterpollo_core::session::EVENT_PERIOD);
                        }
                    }
                    if !poll_feedback
                        && let Some(i) = &mut p.injector
                        && let Err(e) = i.due()
                    {
                        s.launch.warnings.event("input_repeat", format!("Input release or repeat failed ({e:#}); held controls may not update. Unlock the desktop or reconnect."), butterpollo_core::session::EVENT_PERIOD);
                    }
                    if poll_feedback
                        && let Some(i) = &mut p.injector
                        && let Err(e) = i.refresh()
                    {
                        s.launch.warnings.event("input_pointer", format!("Touch/pen pointer refresh failed ({e:#}); pointer updates may be missing. Unlock the desktop or disable native touch/pen if Windows does not support it."), butterpollo_core::session::EVENT_PERIOD);
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
                            let _ = host.peer_mut(*peer_id).send(
                                FEEDBACK_CHANNEL,
                                &Packet::new(message, PacketKind::Reliable),
                            );
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
            // A session whose client never connected control, or whose peer
            // this worker lost when it restarted, would otherwise hold the
            // encoder, capture and display forever (Sunshine times it out).
            let active: Vec<_> = h
                .sessions
                .lock()
                .unwrap()
                .active
                .iter()
                .map(|(id, s)| (id.clone(), s.clone()))
                .collect();
            unattended.retain(|id, _| active.iter().any(|(active, _)| active == id));
            for (id, s) in active {
                if s.stopping() || peers.values().any(|p| p.id == id) {
                    unattended.remove(&id);
                    continue;
                }
                let since = *unattended.entry(id).or_insert_with(Instant::now);
                if since.elapsed() > ping_timeout {
                    tracing::warn!(client=%s.launch.client.name,"session has no control connection; ending it");
                    s.fail();
                    s.stop();
                }
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
    #[cfg(test)]
    _fixture_input: Option<reconnect_tests::Slot>,
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
    /// Input permissions this device lacked and was warned about.
    denied: u32,
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
const MOTION_EVENT_REQUEST: u16 = 0x5501;
const RUMBLE_TRIGGER_DATA: u16 = 0x5500;
const RUMBLE_DATA: u16 = 0x010b;
const RUMBLE_MARKER: u32 = 0x00c0ffee;
// Channel 0 is available even when an older Moonlight peer negotiates only
// one channel. Sending on channel 1 then fails with InvalidChannel.
const FEEDBACK_CHANNEL: u8 = 0;

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
            (MOTION_EVENT_REQUEST, payload)
        })
        .collect()
}
fn feedback_packets(id: u16, kind: u16, data: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let mut result = vec![];
    if matches!(kind, 4 | 5) && data.len() >= 8 {
        let mut rumble = RUMBLE_MARKER.to_le_bytes().to_vec();
        rumble.extend_from_slice(&id.to_le_bytes());
        rumble.extend_from_slice(&data[..4]);
        result.push((RUMBLE_DATA, rumble));
        if kind == 4 {
            result.push((
                RUMBLE_TRIGGER_DATA,
                [&id.to_le_bytes()[..], &data[4..8]].concat(),
            ));
        } else if kind == 5 && data[7] & 1 != 0 {
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
    fn ignored_config_values_and_host_wide_overrides_are_logged() {
        use crate::state::test_support::Fixture;
        let f = Fixture::new();
        let path = f.host.directory.join("warnings.log");
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_writer(std::sync::Mutex::new(std::fs::File::create(&path).unwrap()))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut config = Config::parse("parity_number=not-a-number").unwrap();
            assert_eq!(config.integer("parity_number", 7), 7);
            apply_overrides(
                &mut config,
                serde_json::json!({"port":50000}).as_object().unwrap(),
            )
            .unwrap();
            assert!(!config.values.contains_key("port"));
        });
        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("parity_number"), "{log}");
        assert!(log.contains("not-a-number"), "{log}");
        assert!(
            log.contains("port") && log.contains("ignoring an override"),
            "{log}"
        );
    }
    #[test]
    fn server_commands_require_permission_app_opt_in_and_a_valid_index_and_are_throttled() {
        use crate::state::test_support::Fixture;
        use serde_json::json;
        let f = Fixture::new();
        let mut client = f.client(1 << 20);
        f.host.config.write().unwrap().values.insert("server_cmd".into(), json!([
            {"name":"First","cmd":"echo first"}, {"name":"Second","cmd":"echo second","elevated":true}
        ]).to_string());
        let mut at = None;
        assert_eq!(
            server_command(&f.host, &client, &[1], &mut at),
            Some(("echo second".into(), true))
        );
        assert!(at.is_some());
        assert_eq!(server_command(&f.host, &client, &[0], &mut at), None);
        at = Some(Instant::now() - Duration::from_secs(2));
        assert_eq!(
            server_command(&f.host, &client, &[0], &mut at),
            Some(("echo first".into(), false))
        );
        for payload in [vec![], vec![0, 1], vec![2]] {
            at = None;
            assert_eq!(server_command(&f.host, &client, &payload, &mut at), None);
        }
        for (permission, enabled) in [(0, true), (1 << 20, false)] {
            client.perm = permission;
            client.enabled = enabled;
            at = None;
            assert_eq!(server_command(&f.host, &client, &[0], &mut at), None);
            assert!(at.is_none());
        }
        client.perm = 1 << 20;
        client.enabled = true;
        let app =
            serde_json::from_value(json!({"name":"No commands","allow-client-commands":false}))
                .unwrap();
        *f.host.current_app.lock().unwrap() =
            Some(crate::process::RunningApp::with_environment(&app, Default::default()).unwrap());
        assert_eq!(server_command(&f.host, &client, &[0], &mut at), None);
        assert!(at.is_none());
    }
    #[test]
    fn app_overrides_win_over_device_and_host_without_changing_saved_configuration() {
        use crate::state::test_support::Fixture;
        use serde_json::json;
        let f = Fixture::new();
        *f.host.config.write().unwrap() =
            Config::parse("max_bitrate=10000\ncapture=ddx\nport=47989\nkeyboard=true").unwrap();
        let original = f.host.config.read().unwrap().values.clone();
        let mut client = f.client(u32::MAX);
        client.extra.insert(
            "config_overrides".into(),
            json!({"max_bitrate":20000,"capture":"wgc","port":50000,"keyboard":false}),
        );
        let launch = f.launch(client, Role::Stream);
        let device = effective_config(&f.host, &launch).unwrap();
        assert_eq!(device.integer("max_bitrate", 0), 20000);
        assert_eq!(device.get("capture", ""), "wgc");
        assert!(!device.boolean("keyboard", true));
        assert_eq!(device.integer("port", 0), 47989);
        f.host.apps.write().unwrap()[0].extra.insert(
            "config-overrides".into(),
            json!({"max_bitrate":30000,"capture":"","port":51000,"keyboard":true}),
        );
        let app = effective_config(&f.host, &launch).unwrap();
        assert_eq!(app.integer("max_bitrate", 0), 30000);
        assert_eq!(app.get("capture", ""), "wgc");
        assert!(app.boolean("keyboard", false));
        assert_eq!(app.integer("port", 0), 47989);
        assert_eq!(f.host.config.read().unwrap().values, original);
        assert_eq!(
            launch.client.extra["config_overrides"]["max_bitrate"],
            20000
        );
        assert_eq!(
            f.host.apps.read().unwrap()[0].extra["config-overrides"]["port"],
            51000
        );
    }
    #[test]
    fn capture_waits_for_its_display_and_shows_the_primary_only_after_the_timeout() {
        let start = Instant::now();
        let mut missing = None;
        assert_eq!(reopen_on(true, &mut missing, start), ReopenOn::Stream);
        // Windows switched the virtual display off: capture waits for the
        // heartbeat to bring it back instead of opening the physical desktop.
        assert_eq!(reopen_on(false, &mut missing, start), ReopenOn::Wait);
        let almost = start + DISPLAY_RETURN_WAIT - Duration::from_millis(1);
        assert_eq!(reopen_on(false, &mut missing, almost), ReopenOn::Wait);
        assert_eq!(
            reopen_on(false, &mut missing, start + DISPLAY_RETURN_WAIT),
            ReopenOn::Primary
        );
        // It came back under another name: the wait starts over next time.
        let back = start + Duration::from_secs(20);
        assert_eq!(reopen_on(true, &mut missing, back), ReopenOn::Stream);
        assert_eq!(missing, None);
        assert_eq!(reopen_on(false, &mut missing, back), ReopenOn::Wait);
    }
    #[test]
    fn pad_feedback_forwards_rumble_and_each_family_extra() {
        let rumble = (
            RUMBLE_DATA,
            vec![0xee, 0xff, 0xc0, 0, 2, 0, 255, 255, 128, 128],
        );
        // Xbox pads add trigger rumble.
        let xbox = feedback_packets(2, 4, &[255, 255, 128, 128, 1, 2, 3, 4]);
        assert_eq!(
            xbox,
            vec![
                rumble.clone(),
                (RUMBLE_TRIGGER_DATA, vec![2, 0, 1, 2, 3, 4])
            ]
        );
        // PlayStation pads add the lightbar only when the report sets it.
        let ds4 = feedback_packets(2, 5, &[255, 255, 128, 128, 12, 34, 56, 1]);
        assert_eq!(ds4, vec![rumble.clone(), (0x5502, vec![2, 0, 12, 34, 56])]);
        assert_eq!(
            feedback_packets(2, 5, &[255, 255, 128, 128, 12, 34, 56, 0]),
            vec![rumble]
        );
        // Reports of no known kind send nothing.
        assert!(feedback_packets(2, 3, &[255; 8]).is_empty());
    }
    #[test]
    fn vhf_rumble_and_stop_keep_the_strengths_in_moonlight_packets() {
        for (kind, size) in [(4, 8), (5, 32)] {
            for strengths in [[0x00, 0x80, 0x00, 0x40], [0; 4]] {
                let mut data = vec![0; size];
                data[..4].copy_from_slice(&strengths);
                let packets = feedback_packets(0, kind, &data);
                assert_eq!(packets[0].0, RUMBLE_DATA);
                assert_eq!(RUMBLE_DATA, 0x010b);
                assert_eq!(&packets[0].1[..4], &RUMBLE_MARKER.to_le_bytes());
                assert_eq!(&packets[0].1[4..], &[&[0, 0][..], &strengths].concat());
            }
        }
    }
    #[test]
    fn rumble_reaches_a_single_channel_moonlight_peer() {
        let settings = || HostSettings {
            peer_limit: 1,
            channel_limit: 1,
            ..Default::default()
        };
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        let mut host = Host::new(ControlSocket::new(socket), settings()).unwrap();
        let mut client = Host::new(UdpSocket::bind("127.0.0.1:0").unwrap(), settings()).unwrap();
        client.connect(address, 1, 0).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !host.peer(PeerID(0)).connected() || !client.peer(PeerID(0)).connected() {
            host.service().unwrap();
            client.service().unwrap();
            assert!(Instant::now() < deadline, "ENet connect timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(host.peer(PeerID(0)).channel_count(), 1);
        // The previous channel silently discarded every feedback message.
        assert_eq!(
            host.peer_mut(PeerID(0))
                .send(1, &Packet::new(&[][..], PacketKind::Reliable)),
            Err(rusty_enet::error::PeerSendError::InvalidChannel)
        );
        let key = [0x42; 16];
        for v2 in [false, true] {
            for strengths in [[0x00, 0x80, 0x00, 0x40], [0; 4]] {
                let mut data = vec![0; 8];
                data[..4].copy_from_slice(&strengths);
                let (kind, payload) = feedback_packets(2, 4, &data).remove(0);
                let message =
                    butterpollo_core::packet::encrypted_control(&key, 7, kind, &payload, v2)
                        .unwrap();
                host.socket_mut().hold();
                host.peer_mut(PeerID(0))
                    .send(
                        FEEDBACK_CHANNEL,
                        &Packet::new(message, PacketKind::Reliable),
                    )
                    .unwrap();
                host.flush();
                host.socket_mut().release().unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                let received = loop {
                    host.service().unwrap();
                    if let Some(Event::Receive {
                        channel_id, packet, ..
                    }) = client.service().unwrap()
                    {
                        assert_eq!(channel_id, 0);
                        break packet;
                    }
                    assert!(Instant::now() < deadline, "rumble delivery timed out");
                    std::thread::sleep(Duration::from_millis(1));
                };
                let bytes = received.data();
                let mut iv = vec![0; if v2 { 12 } else { 16 }];
                iv[0] = 7;
                if v2 {
                    iv[10..].copy_from_slice(b"HC");
                }
                let plain =
                    butterpollo_core::crypto::gcm_open(&key, &iv, &bytes[8..24], &bytes[24..])
                        .unwrap();
                // Moonlight's decrypted V2 header, marker, controller ID,
                // then the low- and high-frequency strengths, all LE.
                assert_eq!(
                    &plain[..10],
                    &[0x0b, 0x01, 10, 0, 0xee, 0xff, 0xc0, 0, 2, 0]
                );
                assert_eq!(&plain[10..], &strengths);
            }
        }
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
        let later = start + ENCODER_RECOVERY + Duration::from_secs(1);
        failing = Some(later);
        encoder_progress(&mut failing, false, later + Duration::from_secs(1)).unwrap();
        assert!(encoder_progress(&mut failing, false, later + ENCODER_RECOVERY).is_err());
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
        assert!(packets.iter().any(|(kind, _)| *kind == RUMBLE_DATA));
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
                (MOTION_EVENT_REQUEST, vec![3, 0, 200, 0, 1]),
                (MOTION_EVENT_REQUEST, vec![3, 0, 200, 0, 2])
            ]
        );
        assert_eq!(
            motion_requests(1, 0x20),
            [(MOTION_EVENT_REQUEST, vec![1, 0, 200, 0, 2])]
        );
        assert!(motion_requests(1, 0x0f).is_empty());
    }
}
