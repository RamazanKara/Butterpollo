//! An ENet client sends reliable input-sized packets on loopback; a server
//! loop shaped like Media::control (drain service(), then apply, flush, then
//! wait_readable(1 ms)) records when each packet reaches the apply step.
//! Usage: enet_input_probe [SAMPLES] [defer]. With `defer` the server holds
//! what ENet sends until after the apply step, as ControlSocket does.
use rusty_enet::{Event, Host, HostSettings, Packet};
use std::{
    net::UdpSocket,
    os::windows::io::AsRawSocket,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[link(name = "winmm")]
unsafe extern "system" {
    fn timeBeginPeriod(period: u32) -> u32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> isize;
    fn SetPriorityClass(process: isize, class: u32) -> i32;
}

fn stats(name: &str, mut v: Vec<f64>) {
    if v.is_empty() {
        println!("{name:48} no samples");
        return;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let mean = v.iter().sum::<f64>() / n as f64;
    println!(
        "{name:48} n={n:5} mean {mean:8.1} p50 {:8.1} p90 {:8.1} p99 {:8.1} max {:9.1} us",
        v[n / 2],
        v[n * 90 / 100],
        v[n * 99 / 100],
        v[n - 1]
    );
}

/// Times every socket call ENet makes; optionally holds sends (ACKs) back
/// until the loop releases them after its apply step.
struct TimedSocket {
    socket: UdpSocket,
    defer: bool,
    held: Vec<(std::net::SocketAddr, Vec<u8>)>,
    send_ns: u64,
    sends: u32,
    recv_ns: u64,
    recvs: u32,
}
impl rusty_enet::Socket for TimedSocket {
    type Address = std::net::SocketAddr;
    type Error = std::io::Error;
    fn init(&mut self, options: rusty_enet::SocketOptions) -> std::io::Result<()> {
        rusty_enet::Socket::init(&mut self.socket, options)
    }
    fn send(&mut self, address: std::net::SocketAddr, buffer: &[u8]) -> std::io::Result<usize> {
        if self.defer {
            self.held.push((address, buffer.to_vec()));
            return Ok(buffer.len());
        }
        let t = Instant::now();
        let r = rusty_enet::Socket::send(&mut self.socket, address, buffer);
        self.send_ns += t.elapsed().as_nanos() as u64;
        self.sends += 1;
        r
    }
    fn receive(
        &mut self,
        buffer: &mut [u8; rusty_enet::MTU_MAX],
    ) -> std::io::Result<Option<(std::net::SocketAddr, rusty_enet::PacketReceived)>> {
        let t = Instant::now();
        let r = rusty_enet::Socket::receive(&mut self.socket, buffer);
        self.recv_ns += t.elapsed().as_nanos() as u64;
        self.recvs += 1;
        r
    }
}

fn main() {
    let defer = std::env::args().any(|a| a == "defer");
    butterpollo_windows::timing::disable_power_throttling();
    unsafe {
        timeBeginPeriod(1);
        SetPriorityClass(GetCurrentProcess(), 0x80); // HIGH_PRIORITY_CLASS
    }
    let samples: usize = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(2000);
    let base = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    butterpollo_windows::net::configure_udp(&socket).unwrap();
    let server_address = socket.local_addr().unwrap();
    let results = Arc::new(Mutex::new(Vec::<(String, Vec<f64>)>::new()));
    let server = {
        let stop = stop.clone();
        let results = results.clone();
        std::thread::spawn(move || {
            let raw = socket.as_raw_socket();
            let mut host = Host::new(
                TimedSocket {
                    socket,
                    defer: false,
                    held: vec![],
                    send_ns: 0,
                    sends: 0,
                    recv_ns: 0,
                    recvs: 0,
                },
                HostSettings {
                    peer_limit: 32,
                    channel_limit: 255,
                    ..Default::default()
                },
            )
            .unwrap();
            let _priority = butterpollo_windows::capture::Priority::input();
            let mut woke = Instant::now();
            let (mut total, mut after_wake, mut drain) = (vec![], vec![], vec![]);
            let (mut first, mut sends, mut send_us, mut recvs, mut recv_us) =
                (vec![], vec![], vec![], vec![], vec![]);
            while !stop.load(Ordering::Acquire) {
                let mut stamps = vec![];
                {
                    let s = host.socket_mut();
                    s.defer = defer;
                    (s.send_ns, s.sends, s.recv_ns, s.recvs) = (0, 0, 0, 0);
                }
                let mut first_event = None;
                for _ in 0..512 {
                    match host.service() {
                        Ok(Some(Event::Receive { packet, .. })) => {
                            first_event.get_or_insert_with(Instant::now);
                            let b = packet.data();
                            if b.len() >= 8 {
                                stamps.push(u64::from_le_bytes(b[..8].try_into().unwrap()));
                            }
                        }
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => break,
                    }
                }
                let drained = Instant::now();
                // Production decrypts, decodes and does per-peer bookkeeping
                // here (~1 us measured separately), then calls SendInput.
                let apply = base.elapsed().as_nanos() as u64;
                for stamp in &stamps {
                    total.push(apply.saturating_sub(*stamp) as f64 / 1000.);
                    after_wake.push(drained.duration_since(woke).as_secs_f64() * 1e6);
                }
                if !stamps.is_empty() {
                    drain.push(stamps.len() as f64);
                    let s = host.socket_mut();
                    first.push(first_event.unwrap().duration_since(woke).as_secs_f64() * 1e6);
                    sends.push(f64::from(s.sends));
                    send_us.push(s.send_ns as f64 / 1000.);
                    recvs.push(f64::from(s.recvs));
                    recv_us.push(s.recv_ns as f64 / 1000.);
                }
                {
                    let s = host.socket_mut();
                    s.defer = false;
                    for (address, bytes) in std::mem::take(&mut s.held) {
                        let _ = s.socket.send_to(&bytes, address);
                    }
                }
                host.flush();
                let _ = butterpollo_windows::net::wait_readable(raw, 1);
                woke = Instant::now();
            }
            *results.lock().unwrap() = vec![
                ("client send+flush -> server apply step".into(), total),
                ("server wake -> end of service() drain".into(), after_wake),
                ("server wake -> first Receive event returned".into(), first),
                ("socket sends inside drain (count)".into(), sends),
                ("socket send time inside drain".into(), send_us),
                ("socket receives inside drain (count)".into(), recvs),
                ("socket receive time inside drain".into(), recv_us),
                ("events per drained pass".into(), drain),
            ];
        })
    };
    // Client: a Moonlight stand-in. Sends one reliable packet, flushes at once
    // (as moonlight-common-c does), services between sends for ACKs.
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut host = Host::new(
        client,
        HostSettings {
            peer_limit: 1,
            channel_limit: 48,
            ..Default::default()
        },
    )
    .unwrap();
    let peer = host.connect(server_address, 48, 0).unwrap().id();
    let connected = Instant::now();
    'connect: while connected.elapsed() < Duration::from_secs(5) {
        while let Ok(Some(event)) = host.service() {
            if matches!(event, Event::Connect { .. }) {
                break 'connect;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let timer = butterpollo_windows::timing::Timer::new().unwrap();
    let mut rng = 0x9e3779b97f4a7c15u64;
    let mut next = Instant::now() + Duration::from_millis(50);
    let mut sent = 0;
    while sent < samples {
        while let Ok(Some(_)) = host.service() {}
        let now = Instant::now();
        if now >= next {
            let stamp = base.elapsed().as_nanos() as u64;
            let mut payload = stamp.to_le_bytes().to_vec();
            payload.resize(36, 0); // encrypted mouse-move sized
            host.peer_mut(peer)
                .send(2, &Packet::reliable(payload.as_slice()))
                .unwrap();
            host.flush();
            sent += 1;
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            next = Instant::now() + Duration::from_micros(300 + rng % 3000);
        } else {
            timer.until(next.min(now + Duration::from_micros(250)));
        }
    }
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_millis(200) {
        while let Ok(Some(_)) = host.service() {}
        std::thread::sleep(Duration::from_millis(1));
    }
    stop.store(true, Ordering::Release);
    server.join().unwrap();
    println!(
        "ENet loopback, server loop shaped like Media::control (HIGH class, 1 ms timer), ACKs {}:",
        if defer {
            "held until after apply"
        } else {
            "sent inside service()"
        }
    );
    for (name, v) in std::mem::take(&mut *results.lock().unwrap()) {
        stats(&name, v);
    }
}
