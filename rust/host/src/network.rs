use crate::state::Shared;
use anyhow::{Context, Result, bail};
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

pub fn encryption_mode(config: &Config, address: IpAddr) -> u32 {
    let lan = match address.to_canonical() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    };
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

pub fn bind_address(config: &Config, override_address: Option<IpAddr>) -> Result<IpAddr> {
    if let Some(address) = override_address {
        return Ok(address);
    }
    let configured = config.get("bind_address", "");
    if !configured.is_empty() {
        return configured.parse().context("invalid bind_address");
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
            let instance = config.get("sunshine_name", "Butterpollo Rust");
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
async fn entries<P: Provider>(gateway: &Gateway<P>) -> Result<Vec<igd_next::PortMappingEntry>> {
    let mut entries = Vec::new();
    for index in 0..4096 {
        match tokio::time::timeout(
            Duration::from_secs(3),
            gateway.get_generic_port_mapping_entry(index),
        )
        .await?
        {
            Ok(entry) => entries.push(entry),
            Err(igd_next::GetGenericPortMappingEntryError::SpecifiedArrayIndexInvalid) => {
                return Ok(entries);
            }
            Err(error) => return Err(error.into()),
        }
    }
    bail!("router mapping table exceeds its limit")
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
    let entries = entries(gateway).await?;
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
    let entries = entries(gateway).await?;
    for mapping in applied {
        if entries.iter().any(|entry| {
            entry.protocol == mapping.protocol
                && entry.external_port == mapping.port
                && owned(entry, local, description)
        }) {
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
        assert_eq!(
            mappings(&Config::default(), Ports::from_base(48123)).len(),
            6
        );
    }
    #[tokio::test]
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
