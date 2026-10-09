//! GPU-free integration matrix through the real HTTP, RTSP, ENet and session
//! workers. Only native preparation/input and capture/encoder output are fake.
//! No service, driver, environment switch or production port is used.
use super::*;
use butterpollo_core::{
    config::Ports,
    crypto::Identity,
    session::Preparation,
    state::{App, Client as PairedClient},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicUsize},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CLOSED: u32 = 0x8003_0023;
const FAILED: u32 = 0x8000_4005;
const WAIT: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(crate) struct Fixture {
    displays: Arc<AtomicUsize>,
    inputs: Arc<AtomicUsize>,
    hold_teardown: AtomicBool,
    in_teardown: AtomicBool,
    reset: Mutex<Option<String>>,
    reset_done: AtomicBool,
}

pub(crate) struct Slot(Arc<AtomicUsize>);
impl Slot {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::AcqRel);
        Self(count.clone())
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Display {
    _slot: Slot,
    fixture: Arc<Fixture>,
}
impl Preparation<crate::display_session::StreamPreparation> for Display {
    fn prepared(&self) -> &crate::display_session::StreamPreparation {
        panic!("the fake display must never enter native display preparation")
    }
    fn into_prepared(self: Box<Self>) -> crate::display_session::StreamPreparation {
        panic!("the fake display must never enter native display preparation")
    }
}
impl Drop for Display {
    fn drop(&mut self) {
        if self.fixture.hold_teardown.load(Ordering::Acquire) {
            self.fixture.in_teardown.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.fixture.hold_teardown.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

impl Fixture {
    pub(crate) fn prepare(self: &Arc<Self>, launch: &Launch) {
        // The normal pending-launch slot and session worker own this lease.
        // Counting it verifies custody and teardown without creating a monitor.
        *launch.preparation.lock().unwrap() = Some(Box::new(Display {
            _slot: Slot::new(&self.displays),
            fixture: self.clone(),
        }));
    }
    pub(crate) fn input(&self) -> Slot {
        Slot::new(&self.inputs)
    }
    pub(super) fn reset_peer(
        &self,
        host: &mut Host<ControlSocket>,
        peers: &HashMap<PeerID, ControlPeer>,
    ) {
        let mut reset = self.reset.lock().unwrap();
        if let Some(id) = reset.as_ref()
            && let Some((peer, _)) = peers.iter().find(|(_, p)| &p.id == id)
        {
            // Reset ENet itself without fabricating a Disconnect event or
            // deleting the host's bookkeeping: cleanup is the worker's job.
            host.peer_mut(*peer).reset();
            reset.take();
            self.reset_done.store(true, Ordering::Release);
        }
    }
    pub(super) fn stream(&self, media: &Media, h: &Shared, s: &Session) -> Result<()> {
        let mut packetizer = VideoPacketizer {
            sequence: 0,
            iv_counter: 0,
            frame: 1,
            packet_size: 1024,
            fec_percent: 0,
            min_fec: 0,
            key: None,
        };
        *s.encoder.write().unwrap() = "reconnect fixture".into();
        while !h.stop.load(Ordering::Acquire) && !s.stopping() {
            let peer = media
                .peers
                .lock()
                .unwrap()
                .get(&(s.launch.id.clone(), false))
                .copied();
            if let Some(peer) = peer {
                // A synthetic encoded access unit still uses the production
                // packetizer and video socket. No decoder or GPU is involved.
                for packet in
                    packetizer.encode_recovery(&[0, 0, 0, 1, 0x65, 0x80], true, false, 0, 0)?
                {
                    media.video.send_to(&packet, peer)?;
                    s.stats.packets.fetch_add(1, Ordering::Relaxed);
                }
                s.stats.frames.fetch_add(1, Ordering::Relaxed);
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

async fn until(message: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out: {message}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn address(port: u16) -> SocketAddr {
    assert!(!(47984..=48010).contains(&port) && !(48518..=48544).contains(&port));
    ([127, 0, 0, 1], port).into()
}
fn udp() -> UdpSocket {
    static NEXT: AtomicU16 = AtomicU16::new(52000);
    loop {
        let port = NEXT.fetch_add(1, Ordering::Relaxed);
        assert!(port < 64000, "fixture UDP port range exhausted");
        if let Ok(socket) = UdpSocket::bind(address(port)) {
            socket.set_nonblocking(true).unwrap();
            return socket;
        }
    }
}
fn free_ports() -> Ports {
    static NEXT: AtomicU16 = AtomicU16::new(22000);
    loop {
        let base = NEXT.fetch_add(32, Ordering::Relaxed);
        assert!(base < 40000, "fixture host port range exhausted");
        let ports = Ports::from_base(base);
        if reserve(ports).is_ok() {
            return ports;
        }
    }
}
fn reserve(ports: Ports) -> Result<(Vec<std::net::TcpListener>, Vec<UdpSocket>)> {
    let tcp = [ports.http, ports.https, ports.web, ports.rtsp]
        .into_iter()
        .map(|p| std::net::TcpListener::bind(address(p)))
        .collect::<std::io::Result<_>>()?;
    let udp = [ports.video, ports.control, ports.audio]
        .into_iter()
        .map(|p| UdpSocket::bind(address(p)))
        .collect::<std::io::Result<_>>()?;
    Ok((tcp, udp))
}

struct Harness {
    h: Shared,
    fixture: Arc<Fixture>,
    media: Option<Arc<Media>>,
    listeners: Vec<tokio::task::JoinHandle<Result<()>>>,
    http: reqwest::Client,
    clients: Vec<reqwest::Client>,
    ports: Ports,
    directory: PathBuf,
    started: Instant,
}
impl Harness {
    async fn new(timeout_ms: u32, clients: usize) -> Result<Self> {
        let started = Instant::now();
        let directory =
            std::env::temp_dir().join(format!("butterpollo-reconnect-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let ports = free_ports();
        std::fs::write(
            directory.join("sunshine.conf"),
            format!(
                "port={}\nbind_address=127.0.0.1\nping_timeout={timeout_ms}\nstream_audio=false\nenable_discovery=false\nupnp=false\n",
                ports.http
            ),
        )?;
        let mut h = crate::state::Host::load(directory.clone(), directory.clone(), None)?;
        let fixture = Arc::new(Fixture::default());
        Arc::get_mut(&mut h).unwrap().reconnect_fixture = Some(fixture.clone());
        h.codecs.store(1, Ordering::Release);
        let app = App::desktop();
        *h.apps.write().unwrap() = vec![app.clone()];
        *h.current_app.lock().unwrap() = Some(crate::process::RunningApp::with_environment(
            &app,
            Default::default(),
        )?);
        let mut http_clients = vec![];
        for index in 0..clients {
            let extra_identity;
            let identity = if index == 0 {
                &h.identity
            } else {
                extra_identity = Identity::generate()?;
                &extra_identity
            };
            h.paired.write().unwrap().clients.push(PairedClient {
                name: format!("reconnect-{index}"),
                uuid: index.to_string(),
                cert: identity.certificate.clone(),
                perm: u32::MAX,
                enabled: true,
                extra: Default::default(),
            });
            let pem = format!("{}{}", identity.certificate, identity.private_pem);
            http_clients.push(
                reqwest::Client::builder()
                    .no_proxy()
                    .danger_accept_invalid_certs(true)
                    .identity(reqwest::Identity::from_pem(pem.as_bytes())?)
                    .pool_max_idle_per_host(0)
                    .timeout(Duration::from_secs(8))
                    .build()?,
            );
        }
        let media = Media::new(h.clone(), address(ports.video).ip())?;
        let acceptor = crate::tls::acceptor(&h.identity, true)?;
        let listeners = vec![
            tokio::spawn(crate::tls::serve(
                address(ports.http),
                crate::nvhttp::router(h.clone(), false),
                None,
            )),
            tokio::spawn(crate::tls::serve(
                address(ports.https),
                crate::nvhttp::router(h.clone(), true),
                Some(acceptor),
            )),
            tokio::spawn(crate::rtsp_server::serve(
                address(ports.rtsp),
                h.clone(),
                media.clone(),
            )),
        ];
        let harness = Self {
            h,
            fixture,
            media: Some(media),
            listeners,
            clients: http_clients,
            http: reqwest::Client::builder()
                .no_proxy()
                .pool_max_idle_per_host(0)
                .timeout(WAIT)
                .build()?,
            ports,
            directory,
            started,
        };
        for port in [ports.http, ports.https, ports.rtsp] {
            let deadline = Instant::now() + WAIT;
            loop {
                if tokio::net::TcpStream::connect(address(port)).await.is_ok() {
                    break;
                }
                assert!(Instant::now() < deadline, "listener {port} did not start");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        harness.counts(0, 0).await?;
        Ok(harness)
    }
    async fn launch_response(&self, client: usize, resume: bool) -> Result<String> {
        let endpoint = if resume { "resume" } else { "launch" };
        let key = hex::encode(crypto::random::<16>());
        let app = self.h.apps.read().unwrap()[0].id();
        Ok(self.clients[client]
            .get(format!("https://127.0.0.1:{}/{endpoint}", self.ports.https))
            .query(&[
                ("appid", app.to_string()),
                ("rikey", key),
                ("rikeyid", "1".into()),
                ("corever", "1".into()),
                ("mode", "640x480x60".into()),
            ])
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?)
    }
    async fn launch(&self, client: usize, resume: bool) -> Result<Launch> {
        let response = self.launch_response(client, resume).await?;
        assert!(
            response.contains("status_code=\"200\""),
            "resume={resume}: {response}"
        );
        assert_eq!(
            xml(&response, if resume { "resume" } else { "gamesession" }),
            "1"
        );
        let launch = self
            .h
            .sessions
            .lock()
            .unwrap()
            .pending
            .values()
            .find(|l| l.client.uuid == client.to_string())
            .cloned()
            .expect("launch must be pending");
        Ok(launch)
    }
    async fn rtsp(
        &self,
        launch: &Launch,
        sequence: u32,
        method: &str,
        body: &str,
    ) -> Result<String> {
        let request = format!(
            "{method} rtsp://127.0.0.1/stream RTSP/1.0\r\nCSeq: {sequence}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut iv = [0; 12];
        iv[..4].copy_from_slice(&sequence.to_le_bytes());
        iv[10..].copy_from_slice(b"CR");
        let (tag, ciphertext) = crypto::gcm_seal(&launch.key, &iv, request.as_bytes())?;
        let mut wire = ((ciphertext.len() as u32) | 0x80000000)
            .to_be_bytes()
            .to_vec();
        wire.extend_from_slice(&sequence.to_be_bytes());
        wire.extend_from_slice(&tag);
        wire.extend_from_slice(&ciphertext);
        let response = tokio::time::timeout(WAIT, async {
            let mut socket = tokio::net::TcpStream::connect(address(self.ports.rtsp)).await?;
            socket.write_all(&wire).await?;
            let mut response = vec![];
            socket.read_to_end(&mut response).await?;
            Ok::<_, anyhow::Error>(response)
        })
        .await??;
        assert!(
            response.len() >= 24,
            "RTSP {method} closed without a response"
        );
        let length = u32::from_be_bytes(response[..4].try_into()?) & 0x7fffffff;
        assert_eq!(length as usize, response.len() - 24);
        iv[..4].copy_from_slice(&u32::from_be_bytes(response[4..8].try_into()?).to_le_bytes());
        iv[10..].copy_from_slice(b"HR");
        let response = String::from_utf8(crypto::gcm_open(
            &launch.key,
            &iv,
            &response[8..24],
            &response[24..],
        )?)?;
        assert!(response.starts_with("RTSP/1.0 200"), "{response}");
        Ok(response)
    }
    async fn play(&self, launch: &Launch) -> Result<(Arc<Session>, Client)> {
        self.rtsp(launch, 1, "ANNOUNCE", "a=x-nv-video[0].clientViewportWd:640\r\na=x-nv-video[0].clientViewportHt:480\r\na=x-ss-general.encryptionEnabled:0\r\n").await?;
        self.rtsp(launch, 2, "PLAY", "").await?;
        let session = self.h.sessions.lock().unwrap().active[&launch.id].clone();
        let client = Client::new(self.ports, launch.clone())?;
        until("control and video established", WAIT, || {
            client.frames.load(Ordering::Acquire) > 2
                && session.stats.idr_requests.load(Ordering::Acquire) > 0
        })
        .await;
        assert!(!session.stopping());
        Ok((session, client))
    }
    async fn counts(&self, active: usize, pending: usize) -> Result<()> {
        let response = self
            .http
            .get(format!("http://127.0.0.1:{}/serverinfo", self.ports.http))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        assert_eq!(
            xml(&response, "RustHostSessionCount"),
            active.to_string(),
            "{response}"
        );
        assert_eq!(
            xml(&response, "RustHostPendingSessionCount"),
            pending.to_string(),
            "{response}"
        );
        Ok(())
    }
    async fn empty(&self) -> Result<()> {
        until(
            "sessions, display leases and input slots released",
            WAIT,
            || {
                self.h.sessions.lock().unwrap().idle()
                    && self.fixture.displays.load(Ordering::Acquire) == 0
                    && self.fixture.inputs.load(Ordering::Acquire) == 0
            },
        )
        .await;
        self.counts(0, 0).await?;
        assert!(self.h.sessions.lock().unwrap().teardown.is_empty());
        assert!(self.h.app_display.lock().unwrap().is_empty());
        assert!(self.h.monitors.lock().unwrap().is_empty());
        assert!(
            self.media
                .as_ref()
                .unwrap()
                .peers
                .lock()
                .unwrap()
                .is_empty()
        );
        Ok(())
    }
    async fn end(&self, session: &Session, client: Client) -> Result<()> {
        self.rtsp(&session.launch, 3, "TEARDOWN", "").await?;
        until("normal termination delivered", WAIT, || {
            client.reason.load(Ordering::Acquire) != 0
        })
        .await;
        assert_eq!(session.termination_reason(), CLOSED);
        assert_eq!(client.reason.load(Ordering::Acquire), CLOSED);
        drop(client);
        self.empty().await
    }
    async fn finish(mut self) -> Result<()> {
        self.empty().await?;
        // Reuse the same listeners/UDP ports for a fresh launch before checking
        // that stopping this isolated host releases the sockets themselves.
        let launch = self.launch(0, false).await?;
        let (session, client) = self.play(&launch).await?;
        self.counts(1, 0).await?;
        self.end(&session, client).await?;
        self.h.stop.store(true, Ordering::Release);
        for listener in self.listeners.drain(..) {
            listener.abort();
            assert!(listener.await.unwrap_err().is_cancelled());
        }
        let weak = Arc::downgrade(self.media.as_ref().unwrap());
        self.media.take();
        until("media workers and sockets stopped", WAIT, || {
            weak.upgrade().is_none()
        })
        .await;
        let sockets = reserve(self.ports)?;
        drop(sockets);
        eprintln!(
            "reconnect matrix case completed in {:.3}s",
            self.started.elapsed().as_secs_f64()
        );
        Ok(())
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.fixture.hold_teardown.store(false, Ordering::Release);
        self.h.stop.store(true, Ordering::Release);
        self.h.sessions.lock().unwrap().request_stop(None);
        for listener in &self.listeners {
            listener.abort();
        }
        let weak = self.media.as_ref().map(Arc::downgrade);
        self.media.take();
        let deadline = Instant::now() + WAIT;
        while weak.as_ref().is_some_and(|w| w.upgrade().is_some()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn xml<'a>(text: &'a str, tag: &str) -> &'a str {
    text.split_once(&format!("<{tag}>"))
        .unwrap()
        .1
        .split_once(&format!("</{tag}>"))
        .unwrap()
        .0
}

struct LossySocket {
    socket: UdpSocket,
    blocked: Arc<AtomicBool>,
}
impl rusty_enet::Socket for LossySocket {
    type Address = SocketAddr;
    type Error = std::io::Error;
    fn init(&mut self, options: rusty_enet::SocketOptions) -> std::io::Result<()> {
        rusty_enet::Socket::init(&mut self.socket, options)
    }
    fn send(&mut self, address: SocketAddr, buffer: &[u8]) -> std::io::Result<usize> {
        if self.blocked.load(Ordering::Acquire) {
            return Ok(buffer.len());
        }
        rusty_enet::Socket::send(&mut self.socket, address, buffer)
    }
    fn receive(
        &mut self,
        buffer: &mut [u8; rusty_enet::MTU_MAX],
    ) -> std::io::Result<Option<(SocketAddr, rusty_enet::PacketReceived)>> {
        let packet = rusty_enet::Socket::receive(&mut self.socket, buffer)?;
        if self.blocked.load(Ordering::Acquire) {
            Ok(None)
        } else {
            Ok(packet)
        }
    }
}
struct Client {
    control_down: Arc<AtomicBool>,
    video_down: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    frames: Arc<AtomicUsize>,
    reason: Arc<AtomicU32>,
    worker: Option<thread::JoinHandle<Result<()>>>,
}
impl Client {
    fn new(ports: Ports, launch: Launch) -> Result<Self> {
        let control_down = Arc::new(AtomicBool::new(false));
        let video_down = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(AtomicUsize::new(0));
        let reason = Arc::new(AtomicU32::new(0));
        let mut host = Host::new(
            LossySocket {
                socket: udp(),
                blocked: control_down.clone(),
            },
            HostSettings {
                peer_limit: 1,
                channel_limit: 48,
                ..Default::default()
            },
        )?;
        let peer = host.connect(address(ports.control), 48, launch.connect_data)?;
        peer.set_timeout(32, 60_000, 60_000);
        let peer_id = peer.id();
        let video = udp();
        let (worker_stop, worker_video, worker_frames, worker_reason) = (
            stop.clone(),
            video_down.clone(),
            frames.clone(),
            reason.clone(),
        );
        let worker = thread::spawn(move || -> Result<()> {
            let mut next_ping = Instant::now();
            let mut buffer = [0; 2048];
            while !worker_stop.load(Ordering::Acquire) {
                while let Some(event) = host.service()? {
                    if let Event::Receive { packet, .. } = event {
                        let (_, kind, payload) = butterpollo_core::packet::decrypt_control(
                            &launch.key,
                            packet.data(),
                            false,
                        )?;
                        if kind == 0x0109 {
                            worker_reason.store(
                                u32::from_be_bytes(payload[..4].try_into()?),
                                Ordering::Release,
                            );
                        }
                    }
                }
                if Instant::now() >= next_ping {
                    if host.peer(peer_id).connected() {
                        // An IDR request is also a valid control heartbeat, and
                        // its counter proves the host parsed it after recovery.
                        host.peer_mut(peer_id)
                            .send(0, &Packet::new(&[2, 3][..], PacketKind::Reliable))?;
                    }
                    if !worker_video.load(Ordering::Acquire) {
                        video.send_to(launch.ping.as_bytes(), address(ports.video))?;
                    }
                    next_ping = Instant::now() + Duration::from_millis(50);
                }
                while let Ok((n, source)) = video.recv_from(&mut buffer) {
                    assert_eq!(source, address(ports.video));
                    assert!(n > 16, "video packet has no RTP payload");
                    if !worker_video.load(Ordering::Acquire) {
                        worker_frames.fetch_add(1, Ordering::Relaxed);
                    }
                }
                host.flush();
                thread::sleep(Duration::from_millis(1));
            }
            // Dropping ENet sends no Disconnect, just like a vanished client.
            Ok(())
        });
        Ok(Self {
            control_down,
            video_down,
            stop,
            frames,
            reason,
            worker: Some(worker),
        })
    }
    fn network(&self, down: bool) {
        self.control_down.store(down, Ordering::Release);
        self.video_down.store(down, Ordering::Release);
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap().unwrap();
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vanishes_then_resumes_within_ping_timeout() -> Result<()> {
    let h = Harness::new(10_000, 1).await?;
    let launch = h.launch(0, false).await?;
    let (old, client) = h.play(&launch).await?;
    drop(client);
    assert!(!old.stopping());
    let resumed = h.launch(0, true).await?;
    assert_ne!(resumed.id, old.launch.id);
    assert!(old.stopping());
    assert_eq!(old.termination_reason(), CLOSED);
    h.counts(0, 1).await?;
    assert_eq!(h.fixture.displays.load(Ordering::Acquire), 1);
    let (session, client) = h.play(&resumed).await?;
    h.end(&session, client).await?;
    h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vanishes_then_resumes_after_ping_timeout() -> Result<()> {
    let h = Harness::new(1000, 1).await?;
    let launch = h.launch(0, false).await?;
    let (old, client) = h.play(&launch).await?;
    drop(client);
    until("vanished client times out", WAIT, || old.stopping()).await;
    assert_eq!(old.termination_reason(), FAILED);
    h.empty().await?;
    let resumed = h.launch(0, true).await?;
    let (session, client) = h.play(&resumed).await?;
    h.end(&session, client).await?;
    h.finish().await
}

async fn network_drop(seconds: u64, timeout_ms: u32, survives: bool) -> Result<()> {
    let h = Harness::new(timeout_ms, 1).await?;
    let launch = h.launch(0, false).await?;
    let (session, client) = h.play(&launch).await?;
    let lost_at = Instant::now();
    client.network(true);
    if !survives {
        until(
            "outage ends the session within ping_timeout plus worker scheduling allowance",
            Duration::from_millis(u64::from(timeout_ms)) + WAIT,
            || session.stopping(),
        )
        .await;
        assert_eq!(session.termination_reason(), FAILED);
        h.empty().await?;
    }
    tokio::time::sleep(Duration::from_secs(seconds).saturating_sub(lost_at.elapsed())).await;
    if survives {
        assert!(
            !session.stopping(),
            "{seconds}s outage ended the session within ping_timeout"
        );
        let before = session.stats.idr_requests.load(Ordering::Acquire);
        let frames = client.frames.load(Ordering::Acquire);
        client.network(false);
        until("control and video resume after packet loss", WAIT, || {
            session.stats.idr_requests.load(Ordering::Acquire) > before
                && client.frames.load(Ordering::Acquire) > frames
        })
        .await;
        h.counts(1, 0).await?;
        h.end(&session, client).await?;
    } else {
        assert!(
            session.stopping(),
            "{seconds}s outage exceeded ping_timeout without ending"
        );
        assert_eq!(session.termination_reason(), FAILED);
        drop(client);
        h.empty().await?;
        let resumed = h.launch(0, true).await?;
        let (session, client) = h.play(&resumed).await?;
        h.end(&session, client).await?;
    }
    h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_drop_2s_survives_default_timeout() -> Result<()> {
    network_drop(2, 10_000, true).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_drop_10s_survives_15s_timeout() -> Result<()> {
    network_drop(10, 15_000, true).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_drop_40s_times_out_then_resumes() -> Result<()> {
    network_drop(40, 10_000, false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_clients_race_to_resume_the_same_app() -> Result<()> {
    let h = Harness::new(10_000, 2).await?;
    let generation =
        h.h.current_app
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .generation
            .clone();
    let (a, b) = tokio::join!(h.launch(0, true), h.launch(1, true));
    let (a, b) = (a?, b?);
    assert_ne!(a.id, b.id);
    h.counts(0, 2).await?;
    let (a, b) = tokio::join!(h.play(&a), h.play(&b));
    let ((a, ac), (b, bc)) = (a?, b?);
    h.counts(2, 0).await?;
    assert_eq!(h.fixture.displays.load(Ordering::Acquire), 2);
    assert_eq!(h.fixture.inputs.load(Ordering::Acquire), 2);
    assert_eq!(
        h.h.current_app.lock().unwrap().as_ref().unwrap().generation,
        generation
    );
    h.rtsp(&a.launch, 3, "TEARDOWN", "").await?;
    until("first client's resources released", WAIT, || {
        h.fixture.displays.load(Ordering::Acquire) == 1
            && h.fixture.inputs.load(Ordering::Acquire) == 1
    })
    .await;
    assert_eq!(a.termination_reason(), CLOSED);
    assert!(!b.stopping());
    let before = bc.frames.load(Ordering::Acquire);
    until(
        "second client's video survives first client's teardown",
        WAIT,
        || bc.frames.load(Ordering::Acquire) > before,
    )
    .await;
    h.counts(1, 0).await?;
    drop(ac);
    h.end(&b, bc).await?;
    h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rtsp_reconnect_waits_for_previous_teardown() -> Result<()> {
    for (client, resume) in [(0, false), (0, true), (1, false), (1, true)] {
        reconnect_waits_for_previous_teardown(client, resume).await?;
    }
    Ok(())
}

async fn reconnect_waits_for_previous_teardown(client_index: usize, resume: bool) -> Result<()> {
    let h = Harness::new(10_000, 2).await?;
    let launch = h.launch(0, false).await?;
    let (old, client) = h.play(&launch).await?;
    h.fixture.hold_teardown.store(true, Ordering::Release);
    h.rtsp(&launch, 3, "TEARDOWN", "").await?;
    until("previous session enters resource teardown", WAIT, || {
        h.fixture.in_teardown.load(Ordering::Acquire)
    })
    .await;
    assert!(old.stopping());
    assert_eq!(old.termination_reason(), CLOSED);
    h.counts(0, 0).await?;
    assert!(
        h.h.sessions
            .lock()
            .unwrap()
            .teardown
            .contains_key(&launch.id)
    );
    let (session, resumed_client, overlapped) = {
        let resume = h.launch(client_index, resume);
        tokio::pin!(resume);
        let launch = match tokio::time::timeout(Duration::from_millis(500), &mut resume).await {
            Ok(result) => result?,
            Err(_) => {
                h.counts(0, 0).await?;
                assert_eq!(h.fixture.displays.load(Ordering::Acquire), 1);
                h.fixture.hold_teardown.store(false, Ordering::Release);
                resume.await?
            }
        };
        let (session, resumed_client) = h.play(&launch).await?;
        let overlapped = h.fixture.displays.load(Ordering::Acquire) > 1;
        h.fixture.hold_teardown.store(false, Ordering::Release);
        (session, resumed_client, overlapped)
    };
    drop(client);
    h.end(&session, resumed_client).await?;
    h.finish().await?;
    assert!(
        !overlapped,
        "RTSP PLAY started a replacement while the previous session still owned display resources in teardown"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rtsp_reconnect_reports_busy_when_previous_teardown_times_out() -> Result<()> {
    for (client_index, resume) in [(0, false), (0, true), (1, false), (1, true)] {
        let h = Harness::new(10_000, 2).await?;
        let launch = h.launch(0, false).await?;
        let (old, client) = h.play(&launch).await?;
        h.fixture.hold_teardown.store(true, Ordering::Release);
        h.rtsp(&launch, 3, "TEARDOWN", "").await?;
        until("previous session enters resource teardown", WAIT, || {
            h.fixture.in_teardown.load(Ordering::Acquire)
        })
        .await;

        let started = Instant::now();
        let response = h.launch_response(client_index, resume).await?;
        assert!(started.elapsed() >= Duration::from_secs(5));
        assert!(response.contains("status_code=\"503\""), "{response}");
        assert!(
            response.contains("status_message=\"Another stream operation is still running\""),
            "{response}"
        );
        assert_eq!(
            xml(&response, if resume { "resume" } else { "gamesession" }),
            "0"
        );
        assert!(!response.contains("sessionUrl0"), "{response}");
        h.counts(0, 0).await?;
        assert_eq!(h.fixture.displays.load(Ordering::Acquire), 1);
        assert!(
            h.h.sessions
                .lock()
                .unwrap()
                .teardown
                .contains_key(&launch.id)
        );
        assert_eq!(old.termination_reason(), CLOSED);

        h.fixture.hold_teardown.store(false, Ordering::Release);
        drop(client);
        h.empty().await?;
        let launch = h.launch(client_index, resume).await?;
        let (session, client) = h.play(&launch).await?;
        h.end(&session, client).await?;
        h.finish().await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_lost_while_video_continues_times_out() -> Result<()> {
    let h = Harness::new(2000, 1).await?;
    let launch = h.launch(0, false).await?;
    let (session, client) = h.play(&launch).await?;
    client.control_down.store(true, Ordering::Release);
    let before = client.frames.load(Ordering::Acquire);
    tokio::time::sleep(Duration::from_millis(750)).await;
    assert!(client.frames.load(Ordering::Acquire) > before + 10);
    assert!(!session.stopping());
    until(
        "video traffic cannot keep a lost control stream alive",
        WAIT,
        || session.stopping(),
    )
    .await;
    assert_eq!(session.termination_reason(), FAILED);
    drop(client);
    h.empty().await?;
    let launch = h.launch(0, true).await?;
    let (session, client) = h.play(&launch).await?;
    h.end(&session, client).await?;
    h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_enet_peer_reset_mid_stream_times_out_then_resumes() -> Result<()> {
    let h = Harness::new(1000, 1).await?;
    let launch = h.launch(0, false).await?;
    let (session, client) = h.play(&launch).await?;
    *h.fixture.reset.lock().unwrap() = Some(launch.id.clone());
    until("host ENet peer was reset", WAIT, || {
        h.fixture.reset_done.load(Ordering::Acquire)
    })
    .await;
    until("reset peer's session times out", WAIT, || {
        session.stopping()
    })
    .await;
    assert_eq!(session.termination_reason(), FAILED);
    drop(client);
    h.empty().await?;
    let launch = h.launch(0, true).await?;
    let (session, client) = h.play(&launch).await?;
    h.end(&session, client).await?;
    h.finish().await
}
