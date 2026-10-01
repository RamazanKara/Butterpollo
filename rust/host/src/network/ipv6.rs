//! IGDv2 WANIPv6FirewallControl. Leases are finite and IDs belong to this host.
use super::{Mapping, Protocol};
use anyhow::{Context, Result, bail};
use reqwest::{Client, Url};
use std::{
    net::{IpAddr, Ipv6Addr, SocketAddrV6},
    time::{Duration, Instant},
};
use xmltree::{Element, XMLNode};
const SERVICE: &str = "urn:schemas-upnp-org:service:WANIPv6FirewallControl:1";
fn child(element: &Element, name: &str) -> Option<String> {
    element
        .get_child(name)?
        .get_text()
        .map(|text| text.into_owned())
}
fn descendant<'a>(
    element: &'a Element,
    predicate: &impl Fn(&Element) -> bool,
    depth: usize,
) -> Option<&'a Element> {
    if depth > 16 {
        return None;
    }
    if predicate(element) {
        return Some(element);
    }
    element
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .find_map(|child| descendant(child, predicate, depth + 1))
}
fn control_url(bytes: &[u8], location: &Url) -> Result<Url> {
    if bytes.len() > 262144 {
        bail!("UPnP descriptor exceeds its limit");
    }
    let root = Element::parse(bytes)?;
    let service = descendant(
        &root,
        &|node| node.name == "service" && child(node, "serviceType").as_deref() == Some(SERVICE),
        0,
    )
    .context("IGD has no IPv6 firewall service")?;
    let base = match child(&root, "URLBase").filter(|text| !text.trim().is_empty()) {
        Some(base) => location.join(base.trim())?,
        None => location.clone(),
    };
    let url = base.join(
        child(service, "controlURL")
            .context("IPv6 firewall controlURL missing")?
            .trim(),
    )?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str() != location.host_str()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("IPv6 control URL does not belong to the discovered IGD");
    }
    Ok(url)
}
async fn bytes(mut response: reqwest::Response) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if result.len() + chunk.len() > 262144 {
            bail!("UPnP response exceeds its limit");
        }
        result.extend_from_slice(&chunk);
    }
    Ok(result)
}
async fn soap(
    client: &Client,
    url: &Url,
    action: &str,
    arguments: &[(&str, String)],
) -> Result<Element> {
    let fields: String = arguments
        .iter()
        .map(|(key, value)| {
            format!(
                "<{key}>{}</{key}>",
                value
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
            )
        })
        .collect();
    let body = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{SERVICE}\">{fields}</u:{action}></s:Body></s:Envelope>"
    );
    let response = client
        .post(url.clone())
        .header("SOAPAction", format!("\"{SERVICE}#{action}\""))
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .body(body)
        .send()
        .await?;
    let status = response.status();
    let root = Element::parse(bytes(response).await?.as_slice())?;
    if let Some(error) = descendant(&root, &|node| node.name == "UPnPError", 0) {
        bail!(
            "IPv6 UPnP {action} failed: {} {}",
            child(error, "errorCode").unwrap_or_default(),
            child(error, "errorDescription").unwrap_or_default()
        );
    }
    if !status.is_success() {
        bail!("IPv6 UPnP {action} failed ({status})");
    }
    let name = format!("{action}Response");
    descendant(&root, &|node| node.name == name, 0)
        .cloned()
        .context("IPv6 UPnP response action mismatch")
}
pub(super) struct Firewall {
    client: Client,
    url: Url,
    local: Ipv6Addr,
    leases: Vec<(Mapping, u16, Instant)>,
}
impl Firewall {
    async fn from_location(client: Client, location: Url, local: Ipv6Addr) -> Result<Self> {
        let response = client
            .get(location.clone())
            .send()
            .await?
            .error_for_status()?;
        let url = control_url(&bytes(response).await?, &location)?;
        let status = soap(&client, &url, "GetFirewallStatus", &[]).await?;
        if child(&status, "InboundPinholeAllowed").as_deref() != Some("1") {
            bail!("IGD does not permit IPv6 pinholes");
        }
        Ok(Self {
            client,
            url,
            local,
            leases: Vec::new(),
        })
    }
    pub(super) async fn renew(&mut self, mappings: &[Mapping]) -> Result<()> {
        for &mapping in mappings {
            // Re-adding the tuple renews its lease and returns its current ID;
            // this also recovers safely after a router reboot or lease expiry.
            let response = soap(
                &self.client,
                &self.url,
                "AddPinhole",
                &[
                    ("RemoteHost", String::new()),
                    ("RemotePort", "0".into()),
                    ("InternalClient", self.local.to_string()),
                    ("InternalPort", mapping.port.to_string()),
                    (
                        "Protocol",
                        if mapping.protocol == Protocol::TCP {
                            "6"
                        } else {
                            "17"
                        }
                        .into(),
                    ),
                    ("LeaseTime", "120".into()),
                ],
            )
            .await?;
            let id: u16 = child(&response, "UniqueID")
                .context("IPv6 pinhole ID missing")?
                .parse()?;
            self.leases.retain(|(old, _, _)| *old != mapping);
            self.leases
                .push((mapping, id, Instant::now() + Duration::from_secs(120)));
        }
        Ok(())
    }
    pub(super) async fn close(&mut self) -> Result<()> {
        let mut error = None;
        for (_, id, deadline) in self.leases.drain(..) {
            if deadline <= Instant::now() {
                continue;
            }
            if let Err(e) = soap(
                &self.client,
                &self.url,
                "DeletePinhole",
                &[("UniqueID", id.to_string())],
            )
            .await
            {
                error = Some(e);
            }
        }
        match error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
pub(super) async fn discover(bind: IpAddr) -> Result<Firewall> {
    let search_deadline = Instant::now() + Duration::from_secs(10);
    for interface in if_addrs::get_if_addrs()?
        .into_iter()
        .filter(|i| {
            i.oper_status == if_addrs::IfOperStatus::Up
                && matches!(i.ip(), IpAddr::V6(ip) if ip.segments()[0] & 0xe000 == 0x2000)
        })
        .take(8)
    {
        if Instant::now() >= search_deadline {
            break;
        }
        let IpAddr::V6(local) = interface.ip() else {
            continue;
        };
        if local.segments()[0] & 0xe000 != 0x2000
            || (!bind.is_unspecified() && bind != IpAddr::V6(local))
        {
            continue;
        }
        let Some(index) = interface.index else {
            continue;
        };
        let socket = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::DGRAM, None)?;
        socket.set_only_v6(true)?;
        socket.set_multicast_if_v6(index)?;
        socket.set_multicast_hops_v6(2)?;
        socket.bind(&SocketAddrV6::new(local, 0, 0, index).into())?;
        socket.set_nonblocking(true)?;
        let socket = tokio::net::UdpSocket::from_std(socket.into())?;
        let query = format!(
            "M-SEARCH * HTTP/1.1\r\nHOST: [FF02::C]:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: {SERVICE}\r\n\r\n"
        );
        if socket
            .send_to(
                query.as_bytes(),
                SocketAddrV6::new("ff02::c".parse()?, 1900, 0, index),
            )
            .await
            .is_err()
        {
            continue;
        }
        let deadline = (Instant::now() + Duration::from_secs(2)).min(search_deadline);
        let mut response = [0; 4096];
        let client = Client::builder()
            .local_address(IpAddr::V6(local))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()?;
        for _ in 0..16 {
            let Ok(Ok((count, peer))) = tokio::time::timeout(
                deadline.saturating_duration_since(Instant::now()),
                socket.recv_from(&mut response),
            )
            .await
            else {
                break;
            };
            let location = std::str::from_utf8(&response[..count])
                .ok()
                .and_then(|text| {
                    text.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("location")
                            .then(|| Url::parse(value.trim()).ok())
                            .flatten()
                    })
                });
            if let Some(location) = location
                && discovered_location(&location, peer.ip(), local)
                && let Ok(Ok(firewall)) = tokio::time::timeout(
                    search_deadline.saturating_duration_since(Instant::now()),
                    Firewall::from_location(client.clone(), location, local),
                )
                .await
            {
                return Ok(firewall);
            }
        }
    }
    bail!("no IPv6 firewall gateway discovered")
}
fn discovered_location(location: &Url, peer: IpAddr, local: Ipv6Addr) -> bool {
    if !matches!(location.scheme(), "http" | "https")
        || !location.username().is_empty()
        || location.password().is_some()
    {
        return false;
    }
    let Some(host) = location.host_str().and_then(|h| {
        h.trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<Ipv6Addr>()
            .ok()
    }) else {
        return false;
    };
    IpAddr::V6(host) == peer
        || matches!(peer, IpAddr::V6(source) if source.is_unicast_link_local() && host.segments()[..4] == local.segments()[..4])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ssdp_descriptor_stays_on_the_responding_ipv6_interface() {
        let local = "2001:db8:1::10".parse().unwrap();
        let peer = "2001:db8:1::1".parse().unwrap();
        assert!(discovered_location(
            &Url::parse("http://[2001:db8:1::1]/root.xml").unwrap(),
            peer,
            local
        ));
        assert!(discovered_location(
            &Url::parse("http://[2001:db8:1::1]/root.xml").unwrap(),
            "fe80::1".parse().unwrap(),
            local
        ));
        for url in [
            "http://[2001:db8:2::1]/root.xml",
            "http://example.invalid/root.xml",
            "file:///root.xml",
            "http://user:pass@[2001:db8:1::1]/root.xml",
        ] {
            assert!(!discovered_location(
                &Url::parse(url).unwrap(),
                "fe80::1".parse().unwrap(),
                local
            ));
        }
    }
    #[tokio::test]
    async fn ipv6_soap_round_trip_renews_and_deletes_only_returned_ids() {
        use axum::{Router, extract::State, http::HeaderMap, routing::get};
        use std::sync::{Arc, Mutex};
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        async fn descriptor() -> String {
            format!(
                "<root><device><serviceList><service><serviceType>{SERVICE}</serviceType><controlURL>/firewall</controlURL></service></serviceList></device></root>"
            )
        }
        async fn handler(
            State(calls): State<Arc<Mutex<Vec<String>>>>,
            headers: HeaderMap,
            body: String,
        ) -> String {
            let action = headers["SOAPAction"]
                .to_str()
                .unwrap()
                .trim_matches('"')
                .split('#')
                .nth(1)
                .unwrap();
            calls.lock().unwrap().push(body.clone());
            let value = match action {
                "GetFirewallStatus" => {
                    "<FirewallEnabled>1</FirewallEnabled><InboundPinholeAllowed>1</InboundPinholeAllowed>"
                }
                "AddPinhole" => {
                    assert!(body.contains("<Protocol>17</Protocol>"));
                    assert!(body.contains("<LeaseTime>120</LeaseTime>"));
                    "<UniqueID>73</UniqueID>"
                }
                "DeletePinhole" => {
                    assert!(body.contains("<UniqueID>73</UniqueID>"));
                    ""
                }
                _ => panic!("unexpected action {action}"),
            };
            format!(
                "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:{action}Response xmlns:u=\"{SERVICE}\">{value}</u:{action}Response></s:Body></s:Envelope>"
            )
        }
        let listener = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
        let url = Url::parse(&format!(
            "http://{}/root.xml",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let router = Router::new()
            .route("/root.xml", get(descriptor))
            .route("/firewall", axum::routing::post(handler))
            .with_state(calls.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let mut firewall = Firewall::from_location(client, url.clone(), Ipv6Addr::LOCALHOST)
            .await
            .unwrap();
        let mappings = [Mapping {
            protocol: Protocol::UDP,
            port: 48132,
        }];
        firewall.renew(&mappings).await.unwrap();
        firewall.renew(&mappings).await.unwrap();
        firewall.close().await.unwrap();
        assert_eq!(calls.lock().unwrap().len(), 4);
        server.abort();
        let foreign = format!(
            "<root><service><serviceType>{SERVICE}</serviceType><controlURL>http://example.invalid/control</controlURL></service></root>"
        );
        assert!(control_url(foreign.as_bytes(), &url).is_err());
    }
}
