use crate::state::Shared;
use anyhow::{Context, Result};
use butterpollo_core::{
    config::Config,
    crypto, input,
    packet::{AudioPacketizer, VideoPacketizer},
    session::{Role, Session},
};

fn client_config(h: &Shared, s: &Session) -> Result<Config> {
    let mut config = h.config.read().unwrap().clone();
    if let Some(overrides) = h
        .apps
        .read()
        .unwrap()
        .iter()
        .find(|a| a.id() == s.launch.app_id)
        .and_then(|app| app.extra.get("config-overrides"))
        .and_then(serde_json::Value::as_object)
    {
        apply_overrides(&mut config, overrides)?;
    }
    if let Some(overrides) = s
        .launch
        .client
        .extra
        .get("config_overrides")
        .and_then(serde_json::Value::as_object)
    {
        apply_overrides(&mut config, overrides)?;
    }
    Ok(config)
}
fn apply_overrides(
    config: &mut Config,
    overrides: &serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    let overrides = overrides
        .iter()
        .filter(|(_, v)| v.as_str() != Some(""))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    config.update(&overrides)
}
use butterpollo_windows::{
    audio::{Loopback, Opus},
    capture::{Capture, ComGuard, Image, Priority},
    encoder::Encoder,
    input::Injector,
};
use rusty_enet::{Event, Host, HostSettings, Packet, PacketKind, PeerID};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr, UdpSocket},
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct Latest {
    image: Mutex<Option<Arc<Image>>>,
    changed: Condvar,
    error: Mutex<Option<String>>,
}
struct Source {
    latest: Arc<Latest>,
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
        let video = UdpSocket::bind((bind, ports.video))?;
        let audio = UdpSocket::bind((bind, ports.audio))?;
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
                                    if s.launch.peer != peer.ip() {
                                        continue;
                                    }
                                    let valid = (n >= 16
                                        && crypto::equal(&b[..16], s.launch.ping.as_bytes()))
                                        || (n == 4
                                            && &b[..4] == b"PING"
                                            && sessions
                                                .active
                                                .values()
                                                .filter(|s| s.launch.peer == peer.ip())
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
    fn capture(&self, output: &str, kind: &str, hdr: bool) -> Result<Arc<Source>> {
        let key = format!("{kind}:{output}:{hdr}");
        let mut captures = self.captures.lock().unwrap();
        if let Some(existing) = captures.get(&key).and_then(Weak::upgrade) {
            return Ok(existing);
        }
        let latest = Arc::new(Latest {
            image: Mutex::new(None),
            changed: Condvar::new(),
            error: Mutex::new(None),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = latest.clone();
        let output = output.to_owned();
        let kind = kind.to_owned();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                let result = (|| -> Result<()> {
                    let _com = ComGuard::new()?;
                    let _priority = Priority::new();
                    let mut capture = match Capture::new_format(&output, &kind, hdr) {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = started_tx.send(Err(e.to_string()));
                            return Ok(());
                        }
                    };
                    let _ = started_tx.send(Ok(()));
                    while !worker_stop.load(Ordering::Acquire) {
                        match capture.next_frame() {
                            Ok(Some(image)) => {
                                *worker.image.lock().unwrap() = Some(Arc::new(image));
                                worker.changed.notify_all();
                            }
                            Ok(None) => thread::sleep(Duration::from_millis(1)),
                            Err(e) => {
                                tracing::warn!(error=%e,"capture restarting");
                                thread::sleep(Duration::from_millis(100));
                                *worker.image.lock().unwrap() = None;
                                capture = Capture::new_format(&output, &kind, hdr)?;
                            }
                        }
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    *worker.error.lock().unwrap() = Some(e.to_string());
                    worker.changed.notify_all();
                    tracing::error!(error=%e,"capture worker stopped");
                }
            })?;
        let latest = Arc::new(Source {
            latest,
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
                    let c = client_config(&h, &s)?;
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
                            .find(|m| m.device_id == output || m.display_name == output)
                            .or_else(|| monitors.iter().find(|m| m.primary))
                            .context("input display unavailable")?;
                        *s.output.write().unwrap() = monitor.display_name.clone();
                        let _client_commands = crate::process::ClientCommands::start(&h, &s)?;
                        while !s.stopping() && !h.stop.load(Ordering::Acquire) {
                            thread::sleep(Duration::from_millis(10));
                        }
                        return Ok(());
                    }
                    let app = h
                        .apps
                        .read()
                        .unwrap()
                        .iter()
                        .find(|a| a.id() == s.launch.app_id)
                        .cloned();
                    let app_option = |key: &str| {
                        app.as_ref()
                            .and_then(|a| a.extra.get(key))
                            .and_then(serde_json::Value::as_str)
                    };
                    let mode = s
                        .launch
                        .client
                        .extra
                        .get("virtual_display_mode")
                        .and_then(serde_json::Value::as_str)
                        .filter(|s| !s.is_empty())
                        .or_else(|| app_option("virtual-display-mode"))
                        .filter(|v| !v.is_empty())
                        .unwrap_or(c.get("virtual_display_mode", "disabled"));
                    let explicit = s
                        .launch
                        .client
                        .extra
                        .get("always_use_virtual_display")
                        .is_some_and(|v| v == true || v == "true")
                        || app.as_ref().is_some_and(|a| {
                            a.extra
                                .get("virtual-display")
                                .and_then(serde_json::Value::as_bool)
                                == Some(true)
                        });
                    let virtual_mode = explicit
                        || (mode != "disabled"
                            && butterpollo_windows::display::virtual_display_available());
                    let client_stream_id = format!("{}:stream", s.launch.client.uuid);
                    let stable_id = if mode == "shared" {
                        "butterpollo-rust-shared"
                    } else {
                        &client_stream_id
                    };
                    let requested_display = c.display_request(s.config.width, s.config.height, s.config.fps)?;
                    let retained = if s.launch.role == Role::RemoteMonitor {
                        Some(crate::remote_display::activate(
                            &h,
                            &s.launch.client.uuid,
                            &s.config,
                        )?)
                    } else {
                        None
                    };
                    let mut display = if retained.is_none() {
                        Some(butterpollo_windows::display::Guard::new(
                            output,
                            virtual_mode,
                            stable_id,
                            s.config.width,
                            s.config.height,
                            s.config.fps,
                            s.config.hdr,
                            requested_display.resolution,
                            requested_display.refresh,
                        )?)
                    } else {
                        None
                    };
                    let output = retained
                        .as_ref()
                        .map(|d| d.output.clone())
                        .or_else(|| display.as_ref().map(|d| d.output.clone()))
                        .context("display lease unavailable")?;
                    *s.output.write().unwrap() = output.clone();
                    let _profile = if s.config.hdr {
                        s.launch.client.extra.get("hdr_profile").and_then(serde_json::Value::as_str).filter(|p| !p.is_empty()).and_then(|selection| {
                            match butterpollo_windows::hdr_profile::Lease::acquire(&output, selection) {
                                Ok(profile) => Some(profile),
                                Err(error) => { tracing::warn!(%error, "selected HDR profile could not be applied"); None }
                            }
                        })
                    } else { None };
                    // Declaration order closes the encoder and joins capture before the display lease is removed.
                    let use_truehdr = s.config.hdr && c.boolean("rtx_hdr", false);
                    let latest = m.capture(
                        &output,
                        c.get("capture", "auto"),
                        s.config.hdr && !use_truehdr,
                    )?;
                    let mut truehdr = if use_truehdr {
                        Some(butterpollo_windows::truehdr::Filter::new(
                            &output,
                            [
                                (c.integer("rtx_hdr_contrast", 0) + 100).clamp(0, 200) as u32,
                                (c.integer("rtx_hdr_saturation", 0) + 100).clamp(0, 200) as u32,
                                c.integer("rtx_hdr_middle_gray", 50).clamp(10, 100) as u32,
                                c.integer("rtx_hdr_peak_brightness", 1000).clamp(400, 2000) as u32,
                            ],
                        )?)
                    } else {
                        None
                    };
                    let mut encoder = Encoder::new(&s.config, c.get("encoder", "auto"), &output)?;
                    let _client_commands = crate::process::ClientCommands::start(&h, &s)?;
                    let audio_m = m.clone();
                    let audio_h = h.clone();
                    let audio_s = s.clone();
                    let audio = thread::Builder::new().name("audio".into()).spawn(move || {
                        if let Err(e) = audio_m.audio(audio_h, audio_s.clone()) {
                            tracing::warn!(error=%e,"audio worker stopped");
                            audio_s.stop();
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
                    let start = Instant::now();
                    let period = Duration::from_secs_f64(1. / f64::from(s.config.fps));
                    let mut due = Instant::now();
                    let mut send_due = due;
                    let result = (|| -> Result<()> {
                        while !s.stopping() && !h.stop.load(Ordering::Acquire) {
                            if let Some(display) = display.as_mut() {
                                display.feed()?;
                            }
                            if let Some(error) = &*latest.error.lock().unwrap() {
                                anyhow::bail!("capture stopped: {error}");
                            }
                            let now = Instant::now();
                            if now < due {
                                timer.until(due);
                            }
                            due = due.max(Instant::now()) + period;
                            let image = {
                                let mut current = latest.image.lock().unwrap();
                                if current.is_none() {
                                    current = latest
                                        .changed
                                        .wait_timeout(current, Duration::from_millis(50))
                                        .unwrap()
                                        .0;
                                }
                                current.clone()
                            };
                            let Some(image) = image else { continue };
                            let peer = m
                                .peers
                                .lock()
                                .unwrap()
                                .get(&(s.launch.id.clone(), false))
                                .copied();
                            let Some(peer) = peer else {
                                if start.elapsed() > Duration::from_secs(10) {
                                    anyhow::bail!("client video ping timed out");
                                }
                                continue;
                            };
                            let begin = Instant::now();
                            let converted = truehdr
                                .as_mut()
                                .map(|filter| filter.apply(&image))
                                .transpose()?;
                            let output = encoder.encode(
                                converted.as_ref().unwrap_or(&image),
                                s.idr.swap(false, Ordering::AcqRel),
                                s.bitrate.load(Ordering::Acquire),
                            )?;
                            let latency = begin.elapsed().as_micros() as u64;
                            s.stats.latency_us.store(latency, Ordering::Relaxed);
                            for frame in output {
                                let timestamp = (start.elapsed().as_secs_f64() * 90000.) as u32;
                                let packets = packetizer.encode(
                                    &frame.bytes,
                                    frame.idr,
                                    timestamp,
                                    latency,
                                )?;
                                let bps = c.integer("pacing_max_bitrate_kbps", 0);
                                let bps = if bps > 0 {
                                    (bps as u64 * 1000)
                                        .max(u64::from(s.bitrate.load(Ordering::Relaxed)) * 1100)
                                } else {
                                    800_000_000
                                };
                                send_due = send_due.max(Instant::now());
                                for p in packets {
                                    let now = Instant::now();
                                    if send_due > now {
                                        timer.until(send_due);
                                    }
                                    let bytes = m.video.send_to(&p, peer)?;
                                    send_due +=
                                        Duration::from_secs_f64(bytes as f64 * 8. / bps as f64);
                                    s.stats.packets.fetch_add(1, Ordering::Relaxed);
                                    s.stats.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
                                }
                                s.stats.frames.fetch_add(1, Ordering::Relaxed);
                            }
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
        let config = client_config(&h, &s)?;
        let muted = !config.boolean("stream_audio", true)
            || (s.launch.role == Role::RemoteMonitor
                && config.boolean("remote_monitor_mute_audio", false));
        let mut capture = if muted {
            None
        } else {
            Some(Loopback::new(s.config.audio_channels as usize)?)
        };
        let directory = std::env::current_exe()?.parent().unwrap().to_owned();
        let mut opus = Opus::new(
            &directory,
            s.config.audio_channels as usize,
            s.config.audio_quality,
        )?;
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
            let peer = self
                .peers
                .lock()
                .unwrap()
                .get(&(s.launch.id.clone(), true))
                .copied();
            let samples = capture
                .as_mut()
                .map(|capture| capture.read(frames))
                .transpose()?
                .flatten();
            if let Some(peer) = peer {
                for packet in p.encode(&opus.encode(samples.as_deref().unwrap_or(&silence))?)? {
                    self.audio.send_to(&packet, peer)?;
                }
            } else if start.elapsed() > Duration::from_secs(10) {
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
        let socket = UdpSocket::bind((self.bind, self.control_port))?;
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
        let mut feedback_at = Instant::now();
        while !h.stop.load(Ordering::Acquire) {
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
                            .filter(|l| l.peer == address.ip())
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
                                    hdr_sent: false,
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
                    if s.stopping() || p.seen.elapsed() > Duration::from_secs(30) {
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
                    if !p.hdr_sent && !s.output.read().unwrap().is_empty() {
                        let mut metadata = vec![u8::from(s.config.hdr)];
                        for value in [
                            35400u16, 14600, 8500, 39850, 6550, 2300, 15635, 16450, 1000, 1, 0, 0,
                            0,
                        ] {
                            metadata.extend_from_slice(&value.to_le_bytes());
                        }
                        if let Ok(message) = p.encrypt(&s, 0x010e, &metadata) {
                            let _ = host
                                .peer_mut(*peer_id)
                                .send(0, &Packet::new(message, PacketKind::Reliable));
                            p.hdr_sent = true;
                        }
                    }
                    if !p.inputs.is_empty() && p.injector.is_none() {
                        let c = h.config.read().unwrap();
                        let output = s.output.read().unwrap().clone();
                        match Injector::new(
                            if output.is_empty() {
                                c.get("output_name", "")
                            } else {
                                &output
                            },
                            c.get("gamepad", "auto"),
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
                                        && i.gamepads.as_ref().is_some_and(|g| g.motion_supported())
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
                        if let Some(i) = &mut p.injector
                            && let Err(e) = i.refresh()
                        {
                            tracing::debug!(error=%e,"pointer refresh failed");
                        }
                        if let Some(g) = p.injector.as_mut().and_then(|i| i.gamepads.as_mut()) {
                            let feedback = g.feedback().unwrap_or_default();
                            for (id, kind, data) in feedback {
                                for (kind, payload) in feedback_packets(id, kind, &data) {
                                    if let Ok(message) = p.encrypt(&s, kind, &payload) {
                                        let _ = host
                                            .peer_mut(*peer_id)
                                            .send(1, &Packet::new(message, PacketKind::Reliable));
                                    }
                                }
                            }
                        }
                    }
                } else if !pending || p.seen.elapsed() > Duration::from_secs(30) {
                    host.peer_mut(*peer_id).disconnect_now(0);
                    remove.push(*peer_id);
                }
            }
            for peer in remove {
                peers.remove(&peer);
            }
            host.flush();
            thread::sleep(Duration::from_millis(1));
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
    hdr_sent: bool,
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
