//! A USB/IP server for one virtual USB device on the loopback interface.
//! A USB/IP client attaches it (on Windows, usbip-win2's signed UDE
//! driver), and the device then looks to the host like one plugged into a
//! USB port, with no driver of ours in the kernel.
//!
//! The protocol is the Linux kernel's (Documentation/usb/usbip_protocol):
//! big-endian `OP_REQ_IMPORT` to attach, then 48-byte `CMD_SUBMIT` and
//! `CMD_UNLINK` headers, answered by `RET_SUBMIT` and `RET_UNLINK`.
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// The bus ID the device is exported under.
pub const BUS_ID: &str = "1-1";
const VERSION: u16 = 0x0111;
const OP_REQ_DEVLIST: u16 = 0x8005;
const OP_REQ_IMPORT: u16 = 0x8003;
const CMD_SUBMIT: u32 = 1;
const CMD_UNLINK: u32 = 2;
const RET_SUBMIT: u32 = 3;
const RET_UNLINK: u32 = 4;
const EPIPE: i32 = -32;
const ECONNRESET: i32 = -104;
/// The largest transfer accepted; HID transfers are at most a few hundred bytes.
const MAX_TRANSFER: usize = 65536;
/// How long a client may take to ask for the device once connected.
const IMPORT_WAIT: Duration = Duration::from_secs(5);
/// A client that stops reading is dropped after this long, so the input
/// thread never waits on it.
const WRITE_WAIT: Duration = Duration::from_secs(1);

/// A control request's setup packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}
impl Setup {
    fn parse(b: &[u8; 8]) -> Self {
        Self {
            request_type: b[0],
            request: b[1],
            value: u16::from_le_bytes([b[2], b[3]]),
            index: u16::from_le_bytes([b[4], b[5]]),
            length: u16::from_le_bytes([b[6], b[7]]),
        }
    }
}

/// What the device list and import replies say about the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbDevice {
    pub vendor: u16,
    pub product: u16,
    pub release: u16,
    /// Class, subclass and protocol of each interface.
    pub interfaces: Vec<(u8, u8, u8)>,
}
impl UsbDevice {
    /// `usbip_usb_device`: a full-speed device in configuration 1.
    fn encode(&self) -> Vec<u8> {
        let mut b = vec![0u8; 256 + 32];
        let path = format!("/sys/devices/butterpollo/usb1/{BUS_ID}");
        b[..path.len()].copy_from_slice(path.as_bytes());
        b[256..256 + BUS_ID.len()].copy_from_slice(BUS_ID.as_bytes());
        for n in [1u32, 2, 2] {
            // bus 1, device 2, USB_SPEED_FULL
            b.extend_from_slice(&n.to_be_bytes());
        }
        for n in [self.vendor, self.product, self.release] {
            b.extend_from_slice(&n.to_be_bytes());
        }
        b.extend_from_slice(&[0, 0, 0, 1, 1, self.interfaces.len() as u8]);
        b
    }
}

/// A virtual USB device. Control requests are answered at once; interrupt
/// IN transfers wait for [`Device::interrupt`] to have data.
pub trait Device: Send + 'static {
    fn describe(&self) -> UsbDevice;
    /// Answers a control request on endpoint 0 with the data to return (empty
    /// for OUT requests); None stalls it. `data` is what an OUT request sent.
    fn control(&mut self, setup: &Setup, data: &[u8]) -> Option<Vec<u8>>;
    /// The next report on an interrupt IN endpoint. None holds that
    /// endpoint's transfers until the client cancels them.
    fn interrupt(&mut self, endpoint: u8) -> Option<Vec<u8>>;
}

/// An interrupt IN transfer waiting for data.
#[derive(Clone, Copy, Debug)]
struct Transfer {
    seqnum: u32,
    endpoint: u8,
    length: usize,
}
#[derive(Default)]
struct Link {
    /// The connected client, for replies.
    stream: Option<TcpStream>,
    /// Whether the client imported the device and URBs flow.
    imported: bool,
    /// Transfers on endpoints that report, oldest first.
    pending: VecDeque<Transfer>,
    /// Transfers on endpoints that never report, kept until unlinked.
    held: Vec<Transfer>,
    /// Whether the device changed since its last report.
    changed: bool,
}
impl Link {
    /// Sends a reply; a client that cannot take it is disconnected.
    fn send(&mut self, b: &[u8]) {
        if let Some(stream) = &mut self.stream
            && stream.write_all(b).is_err()
        {
            let _ = stream.shutdown(Shutdown::Both);
            self.stream = None;
            self.imported = false;
        }
    }
}
struct Shared<D> {
    device: Mutex<D>,
    link: Mutex<Link>,
    wake: Condvar,
    stop: AtomicBool,
    period: Duration,
}

/// One device served on `127.0.0.1` at [`Export::port`] under [`BUS_ID`].
/// A client that disconnects may attach again. Dropping it disconnects the
/// client and stops the server.
pub struct Export<D: Device> {
    shared: Arc<Shared<D>>,
    port: u16,
    threads: Vec<JoinHandle<()>>,
}
impl<D: Device> Export<D> {
    /// Serves `device`, completing an interrupt transfer at least every
    /// `period` while the client waits for one, and at once after
    /// [`Export::changed`].
    pub fn listen(device: D, period: Duration) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let shared = Arc::new(Shared {
            device: Mutex::new(device),
            link: Mutex::default(),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            period,
        });
        let mut threads = vec![];
        let accept = shared.clone();
        threads.push(
            std::thread::Builder::new()
                .name("usbip-accept".into())
                .spawn(move || accept_clients(&accept, &listener))?,
        );
        let tick = shared.clone();
        let ticker = std::thread::Builder::new()
            .name("usbip-reports".into())
            .spawn(move || report(&tick));
        let mut export = Self {
            shared,
            port,
            threads,
        };
        export.threads.push(ticker?);
        Ok(export)
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    /// Whether a client has the device attached.
    pub fn imported(&self) -> bool {
        self.shared.link.lock().unwrap().imported
    }
    pub fn with<R>(&self, f: impl FnOnce(&mut D) -> R) -> R {
        f(&mut self.shared.device.lock().unwrap())
    }
    /// Reports the device's new state without waiting for the period.
    pub fn changed(&self) {
        self.shared.link.lock().unwrap().changed = true;
        self.shared.wake.notify_all();
    }
}
impl<D: Device> Drop for Export<D> {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        {
            let mut link = self.shared.link.lock().unwrap();
            if let Some(stream) = link.stream.take() {
                let _ = stream.shutdown(Shutdown::Both);
            }
            link.imported = false;
        }
        self.shared.wake.notify_all();
        // Wake the accept loop.
        let _ = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)),
            Duration::from_secs(1),
        );
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn accept_clients<D: Device>(shared: &Shared<D>, listener: &TcpListener) {
    for stream in listener.incoming() {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let Ok(stream) = stream else { continue };
        if !stream.peer_addr().is_ok_and(|a| a.ip().is_loopback()) {
            continue;
        }
        if let Err(error) = serve(shared, stream) {
            tracing::debug!(%error, "USB/IP client disconnected");
        }
        let mut link = shared.link.lock().unwrap();
        link.stream = None;
        link.imported = false;
        link.pending.clear();
        link.held.clear();
    }
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}
fn op_reply(code: u16, status: u32) -> Vec<u8> {
    let mut b = VERSION.to_be_bytes().to_vec();
    b.extend_from_slice(&code.to_be_bytes());
    b.extend_from_slice(&status.to_be_bytes());
    b
}
/// `RET_SUBMIT` or `RET_UNLINK`: 48 bytes, then any IN data.
fn ret(command: u32, seqnum: u32, status: i32, data: Option<&[u8]>, length: usize) -> Vec<u8> {
    let mut b = Vec::with_capacity(48 + data.map_or(0, <[u8]>::len));
    b.extend_from_slice(&command.to_be_bytes());
    b.extend_from_slice(&seqnum.to_be_bytes());
    b.extend_from_slice(&[0; 12]);
    b.extend_from_slice(&status.to_be_bytes());
    if command == RET_SUBMIT {
        b.extend_from_slice(&(length as u32).to_be_bytes());
        // start frame, number of packets (none: not isochronous), errors
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&u32::MAX.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
    }
    b.resize(48, 0);
    if let Some(data) = data {
        b.extend_from_slice(data);
    }
    b
}

fn serve<D: Device>(shared: &Shared<D>, mut stream: TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(WRITE_WAIT))?;
    stream.set_read_timeout(Some(IMPORT_WAIT))?;
    {
        let mut link = shared.link.lock().unwrap();
        if shared.stop.load(Ordering::Acquire) {
            return Ok(());
        }
        link.stream = Some(stream.try_clone()?);
    }
    let mut head = [0u8; 8];
    stream.read_exact(&mut head)?;
    let code = u16::from_be_bytes([head[2], head[3]]);
    let describe = || shared.device.lock().unwrap().describe();
    match code {
        OP_REQ_DEVLIST => {
            let device = describe();
            let mut b = op_reply(0x0005, 0);
            b.extend_from_slice(&1u32.to_be_bytes());
            b.extend_from_slice(&device.encode());
            for (class, subclass, protocol) in device.interfaces {
                b.extend_from_slice(&[class, subclass, protocol, 0]);
            }
            stream.write_all(&b)?;
            return Ok(());
        }
        OP_REQ_IMPORT => {
            let mut busid = [0u8; 32];
            stream.read_exact(&mut busid)?;
            let len = busid.iter().position(|b| *b == 0).unwrap_or(32);
            if &busid[..len] != BUS_ID.as_bytes() {
                stream.write_all(&op_reply(0x0003, 1))?;
                return Ok(());
            }
            let mut b = op_reply(0x0003, 0);
            b.extend_from_slice(&describe().encode());
            stream.write_all(&b)?;
        }
        _ => {
            return Err(io::Error::other(format!(
                "unknown USB/IP request {code:#x}"
            )));
        }
    }
    stream.set_read_timeout(None)?;
    shared.link.lock().unwrap().imported = true;
    tracing::debug!("USB/IP client attached the device");
    loop {
        let mut h = [0u8; 48];
        stream.read_exact(&mut h)?;
        let (command, seqnum, direction, endpoint) =
            (be32(&h, 0), be32(&h, 4), be32(&h, 12), be32(&h, 16));
        match command {
            CMD_SUBMIT => {
                let length = be32(&h, 24) as i32;
                let packets = be32(&h, 32) as i32;
                let length = usize::try_from(length)
                    .ok()
                    .filter(|n| *n <= MAX_TRANSFER)
                    .ok_or_else(|| io::Error::other("USB/IP transfer too large"))?;
                let mut data = vec![];
                if direction == 0 {
                    data.resize(length, 0);
                    stream.read_exact(&mut data)?;
                }
                if (1..=1024).contains(&packets) {
                    // Isochronous: the device has no such endpoint.
                    stream.read_exact(&mut vec![0; 16 * packets as usize])?;
                    let reply = ret(RET_SUBMIT, seqnum, EPIPE, None, 0);
                    shared.link.lock().unwrap().send(&reply);
                    continue;
                }
                let reply = if endpoint == 0 {
                    let setup = Setup::parse(h[40..48].try_into().unwrap());
                    let answer = shared.device.lock().unwrap().control(&setup, &data);
                    match answer {
                        Some(mut answer) if direction == 1 => {
                            answer.truncate(length.min(usize::from(setup.length)));
                            ret(RET_SUBMIT, seqnum, 0, Some(&answer), answer.len())
                        }
                        Some(_) => ret(RET_SUBMIT, seqnum, 0, None, length),
                        None => ret(RET_SUBMIT, seqnum, EPIPE, None, 0),
                    }
                } else if direction == 1 {
                    let mut link = shared.link.lock().unwrap();
                    link.pending.push_back(Transfer {
                        seqnum,
                        endpoint: endpoint as u8,
                        length,
                    });
                    drop(link);
                    shared.wake.notify_all();
                    continue;
                } else {
                    ret(RET_SUBMIT, seqnum, 0, None, length)
                };
                shared.link.lock().unwrap().send(&reply);
            }
            CMD_UNLINK => {
                let victim = be32(&h, 20);
                let mut link = shared.link.lock().unwrap();
                let before = link.pending.len() + link.held.len();
                link.pending.retain(|t| t.seqnum != victim);
                link.held.retain(|t| t.seqnum != victim);
                // A transfer already completed is not cancelled: status 0.
                let found = link.pending.len() + link.held.len() != before;
                let status = if found { ECONNRESET } else { 0 };
                link.send(&ret(RET_UNLINK, seqnum, status, None, 0));
            }
            _ => {
                return Err(io::Error::other(format!(
                    "unknown USB/IP command {command}"
                )));
            }
        }
    }
}

/// Completes interrupt transfers: one per endpoint each period, and at once
/// when the device changed.
fn report<D: Device>(shared: &Shared<D>) {
    let mut next = Instant::now();
    let mut link = shared.link.lock().unwrap();
    loop {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let waiting = link.imported && !link.pending.is_empty();
        let now = Instant::now();
        if !waiting {
            link = shared.wake.wait(link).unwrap();
            continue;
        }
        if !link.changed && now < next {
            link = shared.wake.wait_timeout(link, next - now).unwrap().0;
            continue;
        }
        let mut device = shared.device.lock().unwrap();
        let mut served = 0u32;
        let mut sent = false;
        let mut i = 0;
        while i < link.pending.len() {
            let transfer = link.pending[i];
            let bit = 1u32 << (transfer.endpoint & 31);
            if served & bit != 0 {
                i += 1;
                continue;
            }
            served |= bit;
            link.pending.remove(i);
            match device.interrupt(transfer.endpoint) {
                Some(mut data) => {
                    data.truncate(transfer.length);
                    let reply = ret(RET_SUBMIT, transfer.seqnum, 0, Some(&data), data.len());
                    link.send(&reply);
                    sent = true;
                }
                None => link.held.push(transfer),
            }
        }
        drop(device);
        if sent {
            link.changed = false;
            next = now + shared.period;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam_deck::{REPORT_PERIOD, SteamDeck};

    fn import(port: u16) -> TcpStream {
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = op_reply(OP_REQ_IMPORT, 0);
        let mut busid = [0u8; 32];
        busid[..3].copy_from_slice(b"1-1");
        request.extend_from_slice(&busid);
        client.write_all(&request).unwrap();
        let mut reply = [0u8; 8 + 312];
        client.read_exact(&mut reply).unwrap();
        assert_eq!(&reply[..8], &op_reply(0x0003, 0)[..]);
        // idVendor and idProduct, big-endian, after path, busid and three u32s.
        assert_eq!(&reply[8 + 300..8 + 304], &[0x28, 0xde, 0x12, 0x05]);
        assert_eq!(reply[8 + 311], 3, "three interfaces");
        client
    }
    fn submit(
        client: &mut TcpStream,
        seqnum: u32,
        endpoint: u32,
        inward: bool,
        length: u32,
        setup: [u8; 8],
        data: &[u8],
    ) {
        let mut b = vec![];
        for n in [
            CMD_SUBMIT,
            seqnum,
            0x0001_0002,
            u32::from(inward),
            endpoint,
            0,
            length,
            0,
            0,
            0,
        ] {
            b.extend_from_slice(&n.to_be_bytes());
        }
        b.extend_from_slice(&setup);
        b.extend_from_slice(data);
        client.write_all(&b).unwrap();
    }
    /// A reply's command, seqnum, status and data.
    fn reply(client: &mut TcpStream) -> (u32, u32, i32, Vec<u8>) {
        let mut h = [0u8; 48];
        client.read_exact(&mut h).unwrap();
        let command = be32(&h, 0);
        let length = if command == RET_SUBMIT {
            be32(&h, 24) as usize
        } else {
            0
        };
        let mut data = vec![0; length];
        // Only IN replies carry data; these tests only ask for IN data.
        client.read_exact(&mut data).unwrap();
        (command, be32(&h, 4), be32(&h, 20) as i32, data)
    }

    #[test]
    fn a_client_imports_the_deck_reads_its_descriptors_and_gets_reports() {
        let export = Export::listen(SteamDeck::new("TEST"), REPORT_PERIOD).unwrap();
        let mut client = import(export.port());
        // GET_DESCRIPTOR(device), 18 bytes.
        submit(
            &mut client,
            1,
            0,
            true,
            18,
            [0x80, 6, 0, 1, 0, 0, 18, 0],
            &[],
        );
        let (command, seqnum, status, data) = reply(&mut client);
        assert_eq!((command, seqnum, status), (RET_SUBMIT, 1, 0));
        assert_eq!(&data[8..12], &[0xde, 0x28, 0x05, 0x12]);
        // A device qualifier stalls.
        submit(
            &mut client,
            2,
            0,
            true,
            10,
            [0x80, 6, 0, 6, 0, 0, 10, 0],
            &[],
        );
        assert_eq!(reply(&mut client).2, EPIPE);
        // SET_REPORT carries data out and returns none.
        let mut ask = [0u8; 64];
        ask[0] = 0xae;
        submit(
            &mut client,
            3,
            0,
            false,
            64,
            [0x21, 9, 0, 3, 2, 0, 64, 0],
            &ask,
        );
        let mut h = [0u8; 48];
        client.read_exact(&mut h).unwrap();
        assert_eq!((be32(&h, 4), be32(&h, 20), be32(&h, 24)), (3, 0, 64));
        assert!(export.imported());

        // The mouse endpoint never reports; the controller reports on its own.
        submit(&mut client, 4, 1, true, 8, [0; 8], &[]);
        submit(&mut client, 5, 3, true, 64, [0; 8], &[]);
        let (_, seqnum, status, data) = reply(&mut client);
        assert_eq!((seqnum, status, data.len()), (5, 0, 64));
        assert_eq!(&data[..4], &[1, 0, 9, 64]);

        // A change goes out at once.
        submit(&mut client, 6, 3, true, 64, [0; 8], &[]);
        let start = Instant::now();
        export.with(|deck| {
            deck.state.apply(&crate::input::Input::Controller {
                id: 0,
                active: 1,
                buttons: 0x1000,
                left_trigger: 0,
                right_trigger: 0,
                sticks: [0; 4],
            })
        });
        export.changed();
        let (_, seqnum, _, data) = reply(&mut client);
        assert_eq!(seqnum, 6);
        assert!(start.elapsed() < Duration::from_secs(1));
        // A periodic report may have raced the change; the next has it.
        let pressed = |data: &[u8]| data[8] & 0x80 != 0;
        if !pressed(&data) {
            submit(&mut client, 7, 3, true, 64, [0; 8], &[]);
            assert!(pressed(&reply(&mut client).3));
        }

        // Unlinking the held mouse transfer cancels it; an unknown one is gone already.
        let mut unlink = vec![];
        for n in [CMD_UNLINK, 8, 0x0001_0002, 0, 1, 4] {
            unlink.extend_from_slice(&n.to_be_bytes());
        }
        unlink.resize(48, 0);
        client.write_all(&unlink).unwrap();
        assert_eq!(reply(&mut client), (RET_UNLINK, 8, ECONNRESET, vec![]));
        unlink[20..24].copy_from_slice(&99u32.to_be_bytes());
        client.write_all(&unlink).unwrap();
        assert_eq!(reply(&mut client).2, 0);

        // Dropping the export disconnects the client promptly.
        drop(export);
        let mut rest = vec![];
        match client.read_to_end(&mut rest) {
            Ok(_) => assert!(rest.is_empty()),
            Err(error) => assert!(!matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            )),
        }
    }

    #[test]
    fn the_device_can_be_listed_and_attached_again_after_a_disconnect() {
        let export = Export::listen(SteamDeck::new("TEST"), REPORT_PERIOD).unwrap();
        let mut list = TcpStream::connect((Ipv4Addr::LOCALHOST, export.port())).unwrap();
        list.write_all(&op_reply(OP_REQ_DEVLIST, 0)).unwrap();
        let mut reply = vec![];
        list.read_to_end(&mut reply).unwrap();
        assert_eq!(reply.len(), 8 + 4 + 312 + 3 * 4);
        assert_eq!(&reply[8..12], &1u32.to_be_bytes());

        let first = import(export.port());
        drop(first);
        let mut second = import(export.port());
        submit(&mut second, 1, 3, true, 64, [0; 8], &[]);
        assert_eq!(reply_len(&mut second), 64);

        // A wrong bus ID is refused.
        let mut wrong = TcpStream::connect((Ipv4Addr::LOCALHOST, export.port())).unwrap();
        let mut request = op_reply(OP_REQ_IMPORT, 0);
        request.extend_from_slice(&[b'9'; 32]);
        wrong.write_all(&request).unwrap();
    }
    fn reply_len(client: &mut TcpStream) -> usize {
        reply(client).3.len()
    }
}
