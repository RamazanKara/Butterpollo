//! Avoid stale UDP ICMP errors aborting a subsequent streaming client.
use anyhow::Result;
use std::{
    net::{SocketAddr, UdpSocket},
    os::windows::io::AsRawSocket,
};
use windows::Win32::Networking::WinSock::*;
/// The PC's DNS host name, which Vibepollo shows to clients by default.
pub fn host_name() -> Option<String> {
    use windows::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
    let mut size = 0u32;
    unsafe {
        let _ = GetComputerNameExW(ComputerNameDnsHostname, None, &mut size);
    }
    if size == 0 || size > 1024 {
        return None;
    }
    let mut name = vec![0u16; size as usize];
    unsafe {
        GetComputerNameExW(
            ComputerNameDnsHostname,
            Some(windows::core::PWSTR(name.as_mut_ptr())),
            &mut size,
        )
        .ok()?;
    }
    name.truncate(size as usize);
    Some(String::from_utf16_lossy(&name)).filter(|name| !name.trim().is_empty())
}
/// Use the interface carrying this connection, including IPv4-mapped IPv6.
/// Loopback and interfaces without an Ethernet address cannot support WOL.
pub fn local_mac(address: std::net::IpAddr) -> Result<String> {
    use windows::Win32::{
        Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS},
        NetworkManagement::IpHelper::*,
    };
    let address = address.to_canonical();
    if address.is_loopback() {
        return Ok("00:00:00:00:00:00".into());
    }
    let mut size = 15000u32;
    for _ in 0..3 {
        if size > 1024 * 1024 {
            anyhow::bail!("network interface table exceeds its limit");
        }
        // The Windows records contain u64 members and pointers.
        let mut storage = vec![0u64; (size as usize).div_ceil(8)];
        let first = storage.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let status = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC.0 as u32,
                GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                None,
                Some(first),
                &mut size,
            )
        };
        if status == ERROR_BUFFER_OVERFLOW.0 {
            continue;
        }
        if status != ERROR_SUCCESS.0 {
            return Err(std::io::Error::from_raw_os_error(status as i32).into());
        }
        unsafe {
            let mut adapter = first;
            while let Some(row) = adapter.as_ref() {
                let mut unicast = row.FirstUnicastAddress;
                while let Some(ip) = unicast.as_ref() {
                    let socket = ip.Address;
                    let family = socket.lpSockaddr.as_ref().map(|a| a.sa_family);
                    let candidate = match family {
                        Some(AF_INET)
                            if socket.iSockaddrLength as usize
                                >= std::mem::size_of::<SOCKADDR_IN>() =>
                        {
                            let value = &*socket.lpSockaddr.cast::<SOCKADDR_IN>();
                            Some(std::net::IpAddr::from(
                                value.sin_addr.S_un.S_addr.to_ne_bytes(),
                            ))
                        }
                        Some(AF_INET6)
                            if socket.iSockaddrLength as usize
                                >= std::mem::size_of::<SOCKADDR_IN6>() =>
                        {
                            let value = &*socket.lpSockaddr.cast::<SOCKADDR_IN6>();
                            Some(std::net::IpAddr::from(value.sin6_addr.u.Byte).to_canonical())
                        }
                        _ => None,
                    };
                    if candidate == Some(address) && row.PhysicalAddressLength == 6 {
                        return Ok(row.PhysicalAddress[..6]
                            .iter()
                            .map(|n| format!("{n:02X}"))
                            .collect::<Vec<_>>()
                            .join(":"));
                    }
                    unicast = ip.Next;
                }
                adapter = row.Next;
            }
        }
        break;
    }
    Ok("00:00:00:00:00:00".into())
}
/// Active Ethernet/Wi-Fi IPv4 addresses for Moonlight's manual Add PC flow.
pub fn lan_addresses() -> Result<Vec<String>> {
    use windows::Win32::{
        Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS},
        NetworkManagement::IpHelper::*,
    };
    let mut size = 15000u32;
    for _ in 0..3 {
        anyhow::ensure!(
            size <= 1024 * 1024,
            "network interface table exceeds its limit"
        );
        let mut storage = vec![0u64; (size as usize).div_ceil(8)];
        let first = storage.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let status = unsafe {
            GetAdaptersAddresses(
                AF_INET.0 as u32,
                GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                None,
                Some(first),
                &mut size,
            )
        };
        if status == ERROR_BUFFER_OVERFLOW.0 {
            continue;
        }
        if status != ERROR_SUCCESS.0 {
            return Err(std::io::Error::from_raw_os_error(status as i32).into());
        }
        let mut addresses = Vec::new();
        unsafe {
            let mut adapter = first;
            while let Some(row) = adapter.as_ref() {
                if row.OperStatus.0 == 1 && matches!(row.IfType, 6 | 71) {
                    let mut interface = MIB_IF_ROW2 {
                        InterfaceLuid: row.Luid,
                        ..Default::default()
                    };
                    let hardware = GetIfEntry2(&mut interface).0 == 0
                        && interface.InterfaceAndOperStatusFlags._bitfield & 1 != 0;
                    let mut unicast = row.FirstUnicastAddress;
                    while let Some(ip) = unicast.as_ref() {
                        if !ip.Address.lpSockaddr.is_null()
                            && ip.Address.iSockaddrLength as usize >= size_of::<SOCKADDR_IN>()
                        {
                            let socket = &*ip.Address.lpSockaddr.cast::<SOCKADDR_IN>();
                            if socket.sin_family == AF_INET {
                                let address = std::net::Ipv4Addr::from(
                                    socket.sin_addr.S_un.S_addr.to_ne_bytes(),
                                );
                                if !address.is_loopback()
                                    && !address.is_link_local()
                                    && !address.is_unspecified()
                                {
                                    addresses.push((hardware, address));
                                }
                            }
                        }
                        unicast = ip.Next;
                    }
                }
                adapter = row.Next;
            }
        }
        // Hyper-V/WSL adapters also identify as Ethernet. Prefer a physical
        // interface so first-run instructions point at a reachable LAN address.
        addresses.sort_by_key(|(hardware, address)| (!hardware, !address.is_private(), *address));
        addresses.dedup_by_key(|(_, address)| *address);
        return Ok(addresses
            .into_iter()
            .map(|(_, address)| address.to_string())
            .collect());
    }
    anyhow::bail!("network interfaces changed during enumeration")
}
pub fn configure_udp(socket: &UdpSocket) -> Result<()> {
    let disabled = 0u32;
    let mut returned = 0;
    if unsafe {
        WSAIoctl(
            SOCKET(socket.as_raw_socket() as usize),
            SIO_UDP_CONNRESET,
            Some((&disabled as *const u32).cast()),
            4,
            None,
            0,
            &mut returned,
            None,
            None,
        )
    } == SOCKET_ERROR
    {
        return Err(std::io::Error::from_raw_os_error(unsafe { WSAGetLastError() }.0).into());
    }
    // Vibepollo's video socket buffer: a key frame's burst fits without the
    // non-blocking socket refusing it.
    socket2::SockRef::from(socket).set_send_buffer_size(1 << 20)?;
    Ok(())
}
/// Send errors after which the socket still works and only these datagrams
/// are lost: a full send buffer, no buffer space, or a route that is down for
/// the moment. Vibepollo drops the packets and keeps streaming; so do we.
fn transient(error: WSA_ERROR) -> bool {
    [
        WSAEWOULDBLOCK,
        WSAENOBUFS,
        WSAENETDOWN,
        WSAENETUNREACH,
        WSAENETRESET,
        WSAEHOSTUNREACH,
        WSAEHOSTDOWN,
        WSAECONNRESET,
        WSAEADDRNOTAVAIL,
    ]
    .contains(&error)
}
/// A send or receive error that loses only this datagram; the socket keeps
/// working. A receive also reports a datagram too large for its buffer this
/// way (WSAEMSGSIZE) after discarding it.
pub fn datagram_lost(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || error.raw_os_error().is_some_and(|code| {
            let code = WSA_ERROR(code);
            transient(code) || code == WSAEMSGSIZE
        })
}
/// How long a send waits for room in a full socket buffer before dropping.
const WRITABLE_WAIT_MS: i32 = 4;
/// Wait until `socket` has a datagram to read, or `timeout_ms` passes;
/// returns whether one is there. Unlike a fixed sleep, input waiting on the
/// socket is handled the moment it arrives.
pub fn wait_readable(
    socket: std::os::windows::io::RawSocket,
    timeout_ms: i32,
) -> std::io::Result<bool> {
    let mut poll = [WSAPOLLFD {
        fd: SOCKET(socket as usize),
        events: POLLRDNORM,
        revents: WSAPOLL_EVENT_FLAGS(0),
    }];
    match unsafe { WSAPoll(poll.as_mut_ptr(), 1, timeout_ms) } {
        ready if ready >= 0 => Ok(ready > 0),
        _ => Err(std::io::Error::from_raw_os_error(
            unsafe { WSAGetLastError() }.0,
        )),
    }
}
fn writable(socket: &UdpSocket) -> bool {
    let mut poll = [WSAPOLLFD {
        fd: SOCKET(socket.as_raw_socket() as usize),
        events: POLLWRNORM,
        revents: WSAPOLL_EVENT_FLAGS(0),
    }];
    (unsafe { WSAPoll(poll.as_mut_ptr(), 1, WRITABLE_WAIT_MS) }) > 0
        && poll[0].revents.0 & POLLWRNORM.0 != 0
}
/// One datagram, retried once when the socket buffer is full. The inner
/// error is Winsock's.
fn send_one(
    socket: &UdpSocket,
    packet: &[u8],
    peer: SocketAddr,
) -> Result<std::result::Result<(), WSA_ERROR>> {
    let send = || {
        socket
            .send_to(packet, peer)
            .map_err(|e| WSA_ERROR(e.raw_os_error().unwrap_or(WSAEINVAL.0)))
    };
    let mut result = send();
    if result == Err(WSAEWOULDBLOCK) && writable(socket) {
        result = send();
    }
    Ok(match result {
        Ok(size) => {
            anyhow::ensure!(
                size == packet.len(),
                "Winsock returned an incomplete UDP datagram"
            );
            Ok(())
        }
        Err(error) => Err(error),
    })
}
/// One datagram; `Ok(false)` when a transient error dropped it.
pub fn send_datagram(socket: &UdpSocket, packet: &[u8], peer: SocketAddr) -> Result<bool> {
    match send_one(socket, packet, peer)? {
        Ok(()) => Ok(true),
        Err(error) if transient(error) => Ok(false),
        Err(error) => Err(std::io::Error::from_raw_os_error(error.0).into()),
    }
}
/// Physical Ethernet speed on the route to this client; zero means unknown.
pub fn routed_link_bps(peer: SocketAddr) -> u64 {
    use windows::Win32::NetworkManagement::{IpHelper::*, Ndis::IfOperStatusUp};
    let peer = SocketAddr::new(peer.ip().to_canonical(), peer.port());
    if peer.ip().is_loopback() {
        return 0;
    }
    let address = socket2::SockAddr::from(peer);
    let mut index = 0;
    unsafe {
        if GetBestInterfaceEx(address.as_ptr().cast(), &mut index) != 0 {
            return 0;
        }
        let mut row = MIB_IF_ROW2 {
            InterfaceIndex: index,
            ..Default::default()
        };
        if GetIfEntry2(&mut row).0 != 0
            || row.Type != 6
            || row.OperStatus != IfOperStatusUp
            || row.InterfaceAndOperStatusFlags._bitfield & 1 == 0
        {
            return 0;
        }
        row.TransmitLinkSpeed
    }
}
/// qWAVE, loaded at run time as Vibepollo does: Windows Server has
/// qwave.dll only with its Quality Windows Audio Video Experience feature.
struct Qwave {
    handle: usize,
    add: QosAdd,
    remove: QosRemove,
}
type QosAdd = unsafe extern "system" fn(
    windows::Win32::Foundation::HANDLE,
    SOCKET,
    *const SOCKADDR,
    i32,
    u32,
    *mut u32,
) -> windows::core::BOOL;
type QosRemove = unsafe extern "system" fn(
    windows::Win32::Foundation::HANDLE,
    SOCKET,
    u32,
    u32,
) -> windows::core::BOOL;
fn qwave() -> Option<&'static Qwave> {
    use windows::Win32::{Foundation::HANDLE, System::LibraryLoader::*};
    #[repr(C)]
    struct Version {
        major: u16,
        minor: u16,
    }
    type Create = unsafe extern "system" fn(*const Version, *mut HANDLE) -> windows::core::BOOL;
    static QWAVE: std::sync::OnceLock<Option<Qwave>> = std::sync::OnceLock::new();
    QWAVE
        .get_or_init(|| unsafe {
            let module = LoadLibraryExW(
                windows::core::w!("qwave.dll"),
                None,
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
            .ok()?;
            let create: Create =
                std::mem::transmute(GetProcAddress(module, windows::core::s!("QOSCreateHandle"))?);
            let add: QosAdd = std::mem::transmute(GetProcAddress(
                module,
                windows::core::s!("QOSAddSocketToFlow"),
            )?);
            let remove: QosRemove = std::mem::transmute(GetProcAddress(
                module,
                windows::core::s!("QOSRemoveSocketFromFlow"),
            )?);
            let mut handle = HANDLE::default();
            if !create(&Version { major: 1, minor: 0 }, &mut handle).as_bool() {
                tracing::warn!(error = %windows::core::Error::from_thread(), "QoS tagging is unavailable");
                return None;
            }
            Some(Qwave {
                handle: handle.0 as usize,
                add,
                remove,
            })
        })
        .as_ref()
}
/// Tags a socket's datagrams to one client for priority, as Vibepollo does
/// when Moonlight asks (it does on a local network): video as audio-video
/// traffic (DSCP 40, 802.1p 5), audio as voice (DSCP 56, 802.1p 7). Wi-Fi
/// sends those from WMM's video and voice queues, ahead of other traffic,
/// and switches that honour the marks do the same. The tag lasts until the
/// flow is dropped.
pub struct QosFlow(u32);
impl QosFlow {
    /// `None` where there is nothing to tag: no qWAVE, a client on this PC,
    /// or an IPv4 client of a dual-stack socket, which qWAVE cannot tag
    /// without connecting the socket that other sessions share.
    pub fn new(socket: &UdpSocket, peer: SocketAddr, voice: bool) -> Result<Option<Self>> {
        const AUDIO_VIDEO: i32 = 3;
        const VOICE: i32 = 4;
        const NON_ADAPTIVE_FLOW: u32 = 2;
        let mapped = matches!(peer.ip(), std::net::IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some());
        if peer.ip().to_canonical().is_loopback() || mapped {
            return Ok(None);
        }
        let Some(qwave) = qwave() else {
            return Ok(None);
        };
        let address = socket2::SockAddr::from(peer);
        let mut flow = 0;
        let added = unsafe {
            (qwave.add)(
                windows::Win32::Foundation::HANDLE(qwave.handle as *mut _),
                SOCKET(socket.as_raw_socket() as usize),
                address.as_ptr().cast(),
                if voice { VOICE } else { AUDIO_VIDEO },
                NON_ADAPTIVE_FLOW,
                &mut flow,
            )
        };
        if !added.as_bool() {
            return Err(windows::core::Error::from_thread().into());
        }
        Ok(Some(Self(flow)))
    }
}
impl Drop for QosFlow {
    fn drop(&mut self) {
        if let Some(qwave) = qwave() {
            unsafe {
                let _ = (qwave.remove)(
                    windows::Win32::Foundation::HANDLE(qwave.handle as *mut _),
                    SOCKET(0),
                    self.0,
                    0,
                );
            }
        }
    }
}
/// Per-message segmentation leaves the shared socket's options untouched.
pub struct Batch {
    offload: bool,
    pub system_calls: u64,
    /// Datagrams lost to transient send errors.
    pub dropped: u64,
    unreported: u64,
    reported: Option<std::time::Instant>,
}
impl Default for Batch {
    fn default() -> Self {
        Self {
            offload: true,
            system_calls: 0,
            dropped: 0,
            unreported: 0,
            reported: None,
        }
    }
}
impl Batch {
    pub fn count(packets: &[Vec<u8>], limit: usize) -> usize {
        let Some(first) = packets.first() else {
            return 0;
        };
        let size = first.len().max(1);
        let limit = limit.min(65507);
        packets
            .iter()
            .take((limit / size).clamp(1, 64))
            .take_while(|p| p.len() == first.len())
            .count()
            .max(1)
    }
    /// Count dropped datagrams and report them at most every 5 seconds.
    fn dropped(&mut self, count: usize, error: WSA_ERROR) {
        self.dropped += count as u64;
        self.unreported += count as u64;
        if self
            .reported
            .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(5))
        {
            tracing::warn!(
                code = error.0,
                dropped = self.unreported,
                "UDP send failed; packets dropped and the client may stutter. Lower bitrate and check the network adapter"
            );
            self.unreported = 0;
            self.reported = Some(std::time::Instant::now());
        }
    }
    /// Bytes handed to Winsock. Datagrams that hit a transient error are
    /// dropped, as Vibepollo does, rather than ending the stream.
    pub fn send(
        &mut self,
        socket: &UdpSocket,
        packets: &[Vec<u8>],
        peer: SocketAddr,
    ) -> Result<usize> {
        if packets.is_empty() {
            return Ok(0);
        }
        if self.offload
            && packets.len() > 1
            && packets.len() <= 64
            && packets.iter().all(|p| p.len() == packets[0].len())
        {
            match self.send_segmented(socket, packets, peer)? {
                Ok(sent) => return Ok(sent),
                Err(error)
                    if [WSAEINVAL, WSAENOPROTOOPT, WSAEOPNOTSUPP, WSAEMSGSIZE].contains(&error) =>
                {
                    self.offload = false;
                    tracing::warn!(
                        code = error.0,
                        "UDP segmentation unavailable; using individual datagrams with more CPU overhead. Update the network driver or lower bitrate if sending stalls"
                    );
                }
                Err(error) if transient(error) => {
                    self.dropped(packets.len(), error);
                    return Ok(0);
                }
                Err(error) => return Err(std::io::Error::from_raw_os_error(error.0).into()),
            }
        }
        let mut sent = 0;
        for packet in packets {
            self.system_calls += 1;
            match send_one(socket, packet, peer)? {
                Ok(()) => sent += packet.len(),
                Err(error) if transient(error) => self.dropped(1, error),
                Err(error) => return Err(std::io::Error::from_raw_os_error(error.0).into()),
            }
        }
        Ok(sent)
    }
    /// One WSASendMsg with UDP segmentation, retried once when the socket
    /// buffer is full. The inner error is Winsock's.
    fn send_segmented(
        &mut self,
        socket: &UdpSocket,
        packets: &[Vec<u8>],
        peer: SocketAddr,
    ) -> Result<std::result::Result<usize, WSA_ERROR>> {
        #[repr(C)]
        struct Control {
            header: CMSGHDR,
            size: u32,
            padding: u32,
        }
        let control = Control {
            header: CMSGHDR {
                cmsg_len: std::mem::size_of::<CMSGHDR>() + 4,
                cmsg_level: IPPROTO_UDP.0,
                cmsg_type: UDP_SEND_MSG_SIZE,
            },
            size: packets[0].len().try_into()?,
            padding: 0,
        };
        let address = socket2::SockAddr::from(peer);
        let mut buffers = [WSABUF {
            len: 0,
            buf: windows::core::PSTR::null(),
        }; 64];
        for (buffer, packet) in buffers.iter_mut().zip(packets) {
            buffer.len = packet.len() as u32;
            buffer.buf = windows::core::PSTR(packet.as_ptr().cast_mut());
        }
        let message = WSAMSG {
            name: address.as_ptr().cast_mut().cast(),
            namelen: address.len(),
            lpBuffers: buffers.as_mut_ptr(),
            dwBufferCount: packets.len() as u32,
            Control: WSABUF {
                len: std::mem::size_of::<Control>() as u32,
                buf: windows::core::PSTR((&control as *const Control).cast_mut().cast()),
            },
            dwFlags: 0,
        };
        let expected: usize = packets.iter().map(Vec::len).sum();
        for attempt in 0..2 {
            let mut sent = 0;
            self.system_calls += 1;
            let result = unsafe {
                WSASendMsg(
                    SOCKET(socket.as_raw_socket() as usize),
                    &message,
                    0,
                    Some(&mut sent),
                    None,
                    None,
                )
            };
            if result == 0 {
                anyhow::ensure!(
                    sent as usize == expected,
                    "Winsock returned an incomplete UDP batch"
                );
                return Ok(Ok(expected));
            }
            let error = unsafe { WSAGetLastError() };
            if error != WSAEWOULDBLOCK || attempt == 1 || !writable(socket) {
                return Ok(Err(error));
            }
        }
        unreachable!("the second attempt always returns")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires a configured native network adapter and route"]
    fn native_local_mac_matches_mapped_ipv6_and_preserves_loopback() -> Result<()> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        // UDP connect selects an interface without sending any packet.
        socket.connect("192.0.2.1:9")?;
        let address = socket.local_addr()?.ip();
        let mac = local_mac(address)?;
        anyhow::ensure!(
            mac != "00:00:00:00:00:00",
            "selected interface has no Ethernet MAC"
        );
        if let std::net::IpAddr::V4(ip) = address {
            assert_eq!(local_mac(ip.to_ipv6_mapped().into())?, mac);
        }
        assert_eq!(local_mac("127.0.0.1".parse()?)?, "00:00:00:00:00:00");
        println!("read-only network identity: {address} / {mac}");
        Ok(())
    }
    #[test]
    fn waiting_for_input_wakes_when_a_datagram_arrives() -> Result<()> {
        use std::os::windows::io::AsRawSocket;
        let receiver = UdpSocket::bind("127.0.0.1:0")?;
        let sender = UdpSocket::bind("127.0.0.1:0")?;
        assert!(!wait_readable(receiver.as_raw_socket(), 0)?);
        sender.send_to(b"input", receiver.local_addr()?)?;
        let waited = std::time::Instant::now();
        assert!(wait_readable(receiver.as_raw_socket(), 1000)?);
        assert!(waited.elapsed() < std::time::Duration::from_millis(500));
        Ok(())
    }
    #[test]
    fn native_udp_batches_preserve_datagrams_destinations_and_short_tail() -> Result<()> {
        for host in ["127.0.0.1:0", "[::1]:0"] {
            let sender = UdpSocket::bind(host)?;
            let receiver = UdpSocket::bind(host)?;
            receiver.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
            let mut packets: Vec<_> = (0..7).map(|i| vec![i; 512]).collect();
            packets.push(vec![91; 13]);
            assert_eq!(Batch::count(&packets, 16384), 7);
            let mut batch = Batch::default();
            assert_eq!(
                batch.send(&sender, &packets[..7], receiver.local_addr()?)?,
                7 * 512
            );
            batch.send(&sender, &packets[7..], receiver.local_addr()?)?;
            for expected in &packets {
                let mut buffer = [0u8; 2048];
                let (size, peer) = receiver.recv_from(&mut buffer)?;
                assert_eq!(&buffer[..size], expected);
                assert_eq!(peer, sender.local_addr()?);
            }
            println!(
                "{host}: {} datagrams, {} send calls",
                packets.len(),
                batch.system_calls
            );
            // Verify the compatibility path explicitly on the same socket.
            batch.offload = false;
            batch.send(&sender, &packets[..2], receiver.local_addr()?)?;
            for expected in &packets[..2] {
                let mut buffer = [0u8; 512];
                let n = receiver.recv(&mut buffer)?;
                assert_eq!(&buffer[..n], expected);
            }
        }
        Ok(())
    }
    #[test]
    fn qos_leaves_clients_it_cannot_tag_alone() -> Result<()> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        assert!(QosFlow::new(&socket, "127.0.0.1:9".parse()?, false)?.is_none());
        let dual = UdpSocket::bind("[::]:0")?;
        assert!(QosFlow::new(&dual, "[::ffff:192.0.2.10]:9".parse()?, true)?.is_none());
        Ok(())
    }
    #[test]
    #[ignore = "sends to a documentation address; run under a packet capture to see the marks"]
    fn qos_flows_tag_a_network_client() -> Result<()> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        for (port, voice) in [(9, false), (10, true)] {
            let peer: SocketAddr = format!("192.0.2.10:{port}").parse()?;
            let flow = QosFlow::new(&socket, peer, voice)?;
            assert!(flow.is_some());
            for _ in 0..5 {
                send_datagram(&socket, b"qos", peer)?;
            }
        }
        Ok(())
    }
    #[test]
    fn transient_send_errors_drop_packets_instead_of_ending_the_stream() -> Result<()> {
        for error in [WSAEWOULDBLOCK, WSAENOBUFS, WSAEHOSTUNREACH, WSAECONNRESET] {
            assert!(transient(error), "{}", error.0);
        }
        for error in [WSAEINVAL, WSAENOTSOCK, WSAEFAULT, WSAEMSGSIZE] {
            assert!(!transient(error), "{}", error.0);
        }
        for code in [WSAENETUNREACH, WSAENETRESET, WSAEMSGSIZE, WSAENOBUFS] {
            assert!(datagram_lost(&std::io::Error::from_raw_os_error(code.0)));
        }
        assert!(!datagram_lost(&std::io::Error::from_raw_os_error(
            WSAENOTSOCK.0
        )));
        // Without SIO_UDP_CONNRESET disabled, the ICMP reply from a closed
        // port turns later sends into WSAECONNRESET on Windows.
        let sender = UdpSocket::bind("127.0.0.1:0")?;
        let closed = UdpSocket::bind("127.0.0.1:0")?.local_addr()?;
        let packets = vec![vec![7u8; 256]; 4];
        let mut batch = Batch::default();
        for _ in 0..20 {
            send_datagram(&sender, &packets[0], closed)?;
            batch.send(&sender, &packets, closed)?;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        configure_udp(&sender)?;
        assert!(socket2::SockRef::from(&sender).send_buffer_size()? >= 1 << 20);
        Ok(())
    }
}
