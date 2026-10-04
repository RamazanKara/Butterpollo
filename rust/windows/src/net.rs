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
    Ok(())
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
/// Per-message segmentation leaves the shared socket's options untouched.
pub struct Batch {
    offload: bool,
    pub system_calls: u64,
}
impl Default for Batch {
    fn default() -> Self {
        Self {
            offload: true,
            system_calls: 0,
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
                let expected: usize = packets.iter().map(Vec::len).sum();
                anyhow::ensure!(
                    sent as usize == expected,
                    "Winsock returned an incomplete UDP batch"
                );
                return Ok(expected);
            }
            let error = unsafe { WSAGetLastError() };
            if [WSAEINVAL, WSAENOPROTOOPT, WSAEOPNOTSUPP, WSAEMSGSIZE].contains(&error) {
                self.offload = false;
                tracing::debug!(
                    code = error.0,
                    "UDP segmentation unavailable; using individual datagrams"
                );
            } else {
                return Err(std::io::Error::from_raw_os_error(error.0).into());
            }
        }
        let mut sent = 0;
        for packet in packets {
            self.system_calls += 1;
            let size = socket.send_to(packet, peer)?;
            anyhow::ensure!(
                size == packet.len(),
                "Winsock returned an incomplete UDP datagram"
            );
            sent += size;
        }
        Ok(sent)
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
}
