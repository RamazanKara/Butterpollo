use crate::state::Shared;
use anyhow::Result;
use butterpollo_core::config::{Config, Ports};
use igd_next::{
    PortMappingProtocol as Protocol,
    aio::{Gateway, Provider},
};
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};
use socket2::{Domain, Socket, Type};
use std::{
    net::{IpAddr, SocketAddr, UdpSocket},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
#[cfg(test)]
mod fixtures;
mod ipv6;

/// Where a peer is, by address, as Vibepollo classifies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reach {
    /// This computer.
    Pc,
    /// Private, link-local and carrier-grade NAT (Tailscale) addresses.
    Lan,
    Wan,
}
pub fn reach(address: IpAddr) -> Reach {
    match address.to_canonical() {
        IpAddr::V4(ip) if ip.is_loopback() => Reach::Pc,
        IpAddr::V4(ip)
            if ip.is_private()
                || ip.is_link_local()
                || (ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64) =>
        {
            Reach::Lan
        }
        IpAddr::V6(ip) if ip.is_loopback() => Reach::Pc,
        IpAddr::V6(ip) if ip.is_unique_local() || ip.is_unicast_link_local() => Reach::Lan,
        _ => Reach::Wan,
    }
}
/// The farthest peers allowed to use the web console and API.
pub fn web_reach(config: &Config) -> Reach {
    match config.get("origin_web_ui_allowed", "lan") {
        "wan" => Reach::Wan,
        "lan" => Reach::Lan,
        _ => Reach::Pc,
    }
}
pub fn encryption_mode(config: &Config, address: IpAddr) -> u32 {
    let lan = reach(address) <= Reach::Lan;
    config
        .integer(
            if lan {
                "lan_encryption_mode"
            } else {
                "wan_encryption_mode"
            },
            if lan { 0 } else { 1 },
        )
        .clamp(0, 2) as u32
}
pub fn ping_timeout(config: &Config) -> Duration {
    Duration::from_millis(config.integer("ping_timeout", 10000).clamp(1000, 300000) as u64)
}

/// The name clients show for this host: `sunshine_name`, else the PC's name.
pub fn host_name(config: &Config) -> String {
    let configured = config.get("sunshine_name", "").trim();
    if !configured.is_empty() {
        return configured.to_owned();
    }
    butterpollo_windows::net::host_name().unwrap_or_else(|| "Rubylight".into())
}
pub fn bind_address(config: &Config, override_address: Option<IpAddr>) -> Result<IpAddr> {
    if let Some(address) = override_address {
        return Ok(address);
    }
    let configured = config.get("bind_address", "");
    if !configured.is_empty() {
        // A value that is no address at all (an interface name) would keep
        // the host, and the console to fix it, down: this PC only until it is
        // corrected. A valid address that is not up yet, as at boot, is kept:
        // the service retries until the network has it.
        return Ok(
            match configured.trim().trim_matches('"').parse::<IpAddr>() {
                Ok(address) => address,
                Err(_) => {
                    let fallback = IpAddr::from([127, 0, 0, 1]);
                    tracing::warn!(
                        bind_address = configured,
                        %fallback,
                        "bind_address is not an IP address; listening on this PC only"
                    );
                    fallback
                }
            },
        );
    }
    match config.get("address_family", "ipv4") {
        "both" => Ok(IpAddr::from([0u16; 8])),
        other => {
            butterpollo_core::config::fallback("address_family", other, "ipv4");
            Ok(IpAddr::from([0, 0, 0, 0]))
        }
    }
}
fn socket(address: SocketAddr, kind: Type) -> Result<Socket> {
    let socket = Socket::new(Domain::for_address(address), kind, None)?;
    if address.is_ipv6() {
        socket.set_only_v6(false)?;
    }
    socket.bind(&address.into())?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}
pub fn udp(address: SocketAddr) -> Result<UdpSocket> {
    Ok(socket(address, Type::DGRAM)?.into())
}
pub fn tcp(address: SocketAddr) -> Result<tokio::net::TcpListener> {
    let socket = socket(address, Type::STREAM)?;
    socket.listen(128)?;
    Ok(tokio::net::TcpListener::from_std(socket.into())?)
}
/// Accepts the next connection. A failed accept belongs to one connection
/// (a client resetting mid-handshake) or is transient (out of handles), so
/// it never ends the listener, which would shut the host and its game down.
pub async fn accept(listener: &tokio::net::TcpListener) -> (tokio::net::TcpStream, SocketAddr) {
    loop {
        match listener.accept().await {
            Ok((socket, peer)) => {
                return (
                    socket,
                    SocketAddr::new(peer.ip().to_canonical(), peer.port()),
                );
            }
            Err(error) if per_connection(&error) => {
                tracing::debug!(%error, "connection closed before it was accepted");
            }
            Err(error) => {
                tracing::warn!(%error, "accepting a connection failed; retrying");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}
fn per_connection(error: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        error.kind(),
        ConnectionReset | ConnectionAborted | ConnectionRefused | Interrupted | WouldBlock
    )
}
pub struct Discovery(ServiceDaemon);
impl Discovery {
    pub fn start(config: &Config, bind: IpAddr) -> Result<Option<Self>> {
        if !config.boolean("enable_discovery", true) || bind.is_loopback() {
            return Ok(None);
        }
        let daemon = ServiceDaemon::new()?;
        let result = (|| -> Result<()> {
            daemon.disable_interface(IfKind::LoopbackV4)?;
            daemon.disable_interface(IfKind::LoopbackV6)?;
            if bind.is_ipv4() {
                daemon.disable_interface(IfKind::IPv6)?;
            }
            if !bind.is_unspecified() {
                daemon.disable_interface(IfKind::All)?;
                daemon.enable_interface(IfKind::Addr(bind))?;
            }
            let hostname = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "butterpollo".into());
            let hostname = format!(
                "{}.local.",
                hostname.trim_end_matches('.').to_ascii_lowercase()
            );
            let instance = host_name(config);
            let instance = instance.as_str();
            let addresses = if bind.is_unspecified() {
                String::new()
            } else {
                bind.to_string()
            };
            let service = ServiceInfo::new(
                "_nvstream._tcp.local.",
                instance,
                &hostname,
                addresses.as_str(),
                config.ports()?.http,
                None,
            )?;
            let service = if bind.is_unspecified() {
                service.enable_addr_auto()
            } else {
                service
            };
            daemon.register(service)?;
            tracing::info!(%instance, "Moonlight discovery registered");
            Ok(())
        })();
        if let Err(error) = result {
            let _ = daemon.shutdown();
            return Err(error);
        }
        Ok(Some(Self(daemon)))
    }
}
impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.0.shutdown();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mapping {
    protocol: Protocol,
    port: u16,
}
fn mappings(config: &Config, ports: Ports) -> Vec<Mapping> {
    let mut mappings = vec![
        Mapping {
            protocol: Protocol::TCP,
            port: ports.http,
        },
        Mapping {
            protocol: Protocol::TCP,
            port: ports.https,
        },
        Mapping {
            protocol: Protocol::TCP,
            port: ports.rtsp,
        },
        Mapping {
            protocol: Protocol::UDP,
            port: ports.video,
        },
        Mapping {
            protocol: Protocol::UDP,
            port: ports.control,
        },
        Mapping {
            protocol: Protocol::UDP,
            port: ports.audio,
        },
    ];
    if config.get("origin_web_ui_allowed", "lan") == "wan" {
        mappings.push(Mapping {
            protocol: Protocol::TCP,
            port: ports.web,
        });
    }
    mappings
}
/// The router's mapping table, and whether it was read to the end. Routers
/// answer the index past the end with 713, but some hide their table (606),
/// answer 714, 402 or 501, or time out: that ends the list without stopping
/// the host from adding its own mappings.
async fn entries<P: Provider>(gateway: &Gateway<P>) -> (Vec<igd_next::PortMappingEntry>, bool) {
    let mut entries = Vec::new();
    for index in 0..4096 {
        match tokio::time::timeout(
            Duration::from_secs(3),
            gateway.get_generic_port_mapping_entry(index),
        )
        .await
        {
            Ok(Ok(entry)) => entries.push(entry),
            Ok(Err(igd_next::GetGenericPortMappingEntryError::SpecifiedArrayIndexInvalid)) => {
                return (entries, true);
            }
            Ok(Err(error)) => {
                tracing::debug!(%error, index, "UPnP router stopped listing its mappings");
                return (entries, false);
            }
            Err(_) => {
                tracing::debug!(index, "UPnP router timed out listing its mappings");
                return (entries, false);
            }
        }
    }
    tracing::debug!("UPnP router mapping table exceeds its limit");
    (entries, false)
}
fn owned(entry: &igd_next::PortMappingEntry, local: IpAddr, description: &str) -> bool {
    entry.internal_client.parse::<IpAddr>().ok() == Some(local)
        && entry.port_mapping_description == description
        && entry.internal_port == entry.external_port
}
async fn apply<P: Provider>(
    gateway: &Gateway<P>,
    local: IpAddr,
    wanted: &[Mapping],
    description: &str,
) -> Result<Vec<Mapping>> {
    let (entries, _) = entries(gateway).await;
    // The stable host description lets a restarted host reclaim its own
    // permanent leases, including ports removed from a later configuration.
    let mut applied: Vec<_> = entries
        .iter()
        .filter(|entry| owned(entry, local, description))
        .map(|entry| Mapping {
            protocol: entry.protocol,
            port: entry.external_port,
        })
        .collect();
    for mapping in wanted {
        if entries.iter().any(|entry| {
            entry.protocol == mapping.protocol
                && entry.external_port == mapping.port
                && !owned(entry, local, description)
        }) {
            tracing::warn!(port=mapping.port, protocol=?mapping.protocol, "UPnP port is already owned by another mapping");
            continue;
        }
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            gateway.add_port(
                mapping.protocol,
                mapping.port,
                (local, mapping.port).into(),
                120,
                description,
            ),
        )
        .await;
        let result = if matches!(
            result,
            Ok(Err(igd_next::AddPortError::OnlyPermanentLeasesSupported))
        ) {
            tokio::time::timeout(
                Duration::from_secs(3),
                gateway.add_port(
                    mapping.protocol,
                    mapping.port,
                    (local, mapping.port).into(),
                    0,
                    description,
                ),
            )
            .await
        } else {
            result
        };
        match result {
            Ok(Ok(())) => {
                if !applied.contains(mapping) {
                    applied.push(*mapping);
                }
            }
            Ok(Err(error)) => tracing::warn!(%error,port=mapping.port,"UPnP mapping failed"),
            Err(error) => tracing::warn!(%error,port=mapping.port,"UPnP mapping timed out"),
        }
    }
    Ok(applied)
}
async fn remove<P: Provider>(
    gateway: &Gateway<P>,
    local: IpAddr,
    applied: &[Mapping],
    description: &str,
) -> Result<()> {
    let (entries, complete) = entries(gateway).await;
    for mapping in applied {
        let entry = entries.iter().find(|entry| {
            entry.protocol == mapping.protocol && entry.external_port == mapping.port
        });
        // A mapping this host added that an unreadable table cannot show is
        // still removed; one the table shows under another owner is not.
        let ours = match entry {
            Some(entry) => owned(entry, local, description),
            None => !complete,
        };
        if ours {
            tokio::time::timeout(
                Duration::from_secs(3),
                gateway.remove_port(mapping.protocol, mapping.port),
            )
            .await??;
        }
    }
    Ok(())
}
pub fn port_forward(h: Shared, bind: IpAddr) -> Option<tokio::task::JoinHandle<()>> {
    if !h.config.read().unwrap().boolean("upnp", false) || bind.is_loopback() {
        return None;
    }
    Some(tokio::spawn(async move {
        let config = h.config.read().unwrap().clone();
        let wanted = mappings(&config, config.ports().unwrap());
        let description = format!("Butterpollo Rust {}", h.paired.read().unwrap().unique_id);
        let mut next_search = Instant::now();
        let mut current = None;
        let mut firewall = None;
        while !h.stop.load(Ordering::Acquire) {
            if Instant::now() >= next_search {
                next_search = Instant::now() + Duration::from_secs(60);
                if current.is_none() {
                    let options = igd_next::SearchOptions {
                        timeout: Some(Duration::from_secs(3)),
                        bind_addr: if bind.is_ipv4() {
                            (bind, 0).into()
                        } else {
                            (IpAddr::from([0, 0, 0, 0]), 0).into()
                        },
                        ..Default::default()
                    };
                    match igd_next::aio::tokio::search_gateway(options).await {
                        Ok(gateway) => {
                            let local = (|| -> Result<IpAddr> {
                                let socket = UdpSocket::bind((IpAddr::from([0, 0, 0, 0]), 0))?;
                                socket.connect(gateway.addr)?;
                                Ok(if bind.is_unspecified() || bind.is_ipv6() {
                                    socket.local_addr()?.ip()
                                } else {
                                    bind
                                })
                            })();
                            if let Ok(local) = local {
                                current = Some((gateway, local, Vec::new()));
                            }
                        }
                        Err(error) => tracing::debug!(%error, "UPnP gateway unavailable; retrying"),
                    }
                }
                if let Some((gateway, local, applied)) = current.as_mut() {
                    match apply(gateway, *local, &wanted, &description).await {
                        Ok(mappings) => *applied = mappings,
                        Err(error) => tracing::warn!(%error, "UPnP mapping could not be renewed"),
                    }
                }
                if bind.is_ipv6() {
                    if firewall.is_none() {
                        firewall = ipv6::discover(bind).await.ok();
                    }
                    if let Some(firewall) = firewall.as_mut()
                        && let Err(error) = firewall.renew(&wanted).await
                    {
                        tracing::debug!(%error, "IPv6 firewall pinholes could not be renewed");
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if let Some(mut firewall) = firewall
            && let Err(error) = firewall.close().await
        {
            tracing::debug!(%error, "IPv6 pinholes will expire after their finite lease");
        }
        if let Some((gateway, local, applied)) = current
            && let Err(error) = remove(&gateway, local, &applied, &description).await
        {
            tracing::warn!(%error, "UPnP cleanup failed; permanent mappings remain reclaimable by this host");
        }
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peers_are_classified_like_vibepollo() {
        for (address, expected) in [
            ("127.0.0.1", Reach::Pc),
            ("::1", Reach::Pc),
            ("192.168.4.20", Reach::Lan),
            ("10.1.2.3", Reach::Lan),
            ("172.20.0.1", Reach::Lan),
            ("169.254.10.1", Reach::Lan),
            ("100.101.102.103", Reach::Lan),
            ("100.128.0.1", Reach::Wan),
            ("fd12::1", Reach::Lan),
            ("fe80::1", Reach::Lan),
            ("::ffff:192.168.1.5", Reach::Lan),
            ("8.8.8.8", Reach::Wan),
            ("2001:db8::1", Reach::Wan),
        ] {
            assert_eq!(reach(address.parse().unwrap()), expected, "{address}");
        }
        assert_eq!(web_reach(&Config::default()), Reach::Lan);
        assert_eq!(
            web_reach(&Config::parse("origin_web_ui_allowed = pc\n").unwrap()),
            Reach::Pc
        );
    }
    #[test]
    fn lan_defaults_and_explicit_binding_keep_legacy_semantics() {
        assert_eq!(
            bind_address(&Config::default(), None).unwrap(),
            "0.0.0.0".parse::<IpAddr>().unwrap()
        );
        let config = Config::parse("address_family=both\nbind_address=192.0.2.10\n").unwrap();
        assert_eq!(
            bind_address(&config, None).unwrap(),
            "192.0.2.10".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            bind_address(&config, Some("127.0.0.1".parse().unwrap())).unwrap(),
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
        // An interface name instead of an address: this PC only, not down.
        let named = Config::parse("bind_address=Ethernet\n").unwrap();
        assert_eq!(
            bind_address(&named, None).unwrap(),
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            mappings(&Config::default(), Ports::from_base(48123)).len(),
            6
        );
    }
    #[tokio::test]
    #[ignore = "listens on every interface; Windows Firewall asks again for every new test binary"]
    async fn dual_stack_listener_accepts_ipv4_and_ipv6_loopback() {
        let listener = tcp("[::]:0".parse().unwrap()).unwrap();
        let port = listener.local_addr().unwrap().port();
        for address in [format!("127.0.0.1:{port}"), format!("[::1]:{port}")] {
            let client = tokio::net::TcpStream::connect(address).await.unwrap();
            let (_, peer) = listener.accept().await.unwrap();
            assert!(peer.ip().to_canonical().is_loopback());
            drop(client);
        }
    }
}
