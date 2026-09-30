//! Avoid stale UDP ICMP errors aborting a subsequent streaming client.
use anyhow::Result;
use std::{net::UdpSocket, os::windows::io::AsRawSocket};
use windows::Win32::Networking::WinSock::*;
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
