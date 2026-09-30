//! Opening the listen port on the router with UPnP IGD, so other users can
//! reach us without manual port forwarding.
//!
//! Mappings get a one-hour lease and are renewed every 30 minutes, so a
//! crashed seekr does not leave a stale rule on the router for long.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

use igd_next::aio::Gateway;
use igd_next::aio::tokio::{Tokio, search_gateway};
use igd_next::{AddPortError, PortMappingProtocol, SearchOptions};

pub const LEASE: Duration = Duration::from_secs(60 * 60);
pub const RENEW_EVERY: Duration = Duration::from_secs(30 * 60);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(4);
const DESCRIPTION: &str = "seekr (Soulseek)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMapping {
    pub port: u16,
    /// The router's public address, if it tells us.
    pub external_ip: Option<IpAddr>,
    /// A rule for this port already existed (e.g. a manual forward).
    pub already_mapped: bool,
}

/// Finds the UPnP router. The discovery request goes straight to the
/// default gateway first: a multicast request's answer comes from another
/// address, so stateful firewalls such as ufw drop it. Multicast is the
/// fallback.
async fn find_gateway() -> Result<Gateway<Tokio>, String> {
    let mut targets = Vec::new();
    if let Some(gw) = default_gateway() {
        targets.push(SocketAddr::new(IpAddr::V4(gw), 1900));
    }
    targets.push(SearchOptions::default().broadcast_address);
    let mut last_error = String::new();
    for target in targets {
        match search_gateway(SearchOptions {
            broadcast_address: target,
            timeout: Some(SEARCH_TIMEOUT),
            ..SearchOptions::default()
        })
        .await
        {
            Ok(gateway) => return Ok(gateway),
            Err(e) => last_error = e.to_string(),
        }
    }
    Err(format!("no UPnP router found ({last_error})"))
}

/// The IPv4 default gateway, from `/proc/net/route` (Linux).
fn default_gateway() -> Option<Ipv4Addr> {
    let table = std::fs::read_to_string("/proc/net/route").ok()?;
    parse_default_gateway(&table)
}

fn parse_default_gateway(table: &str) -> Option<Ipv4Addr> {
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Iface Destination Gateway ...; addresses are little-endian hex.
        if fields.get(1) != Some(&"00000000") {
            return None;
        }
        let raw = u32::from_str_radix(fields.get(2)?, 16).ok()?;
        (raw != 0).then(|| Ipv4Addr::from(raw.swap_bytes()))
    })
}

/// Finds the router and maps TCP `port` to this machine.
pub async fn map(port: u16) -> Result<PortMapping, String> {
    let gateway = find_gateway().await?;

    let local_ip =
        local_ip_towards(gateway.addr).map_err(|e| format!("cannot tell our LAN address: {e}"))?;
    let local = SocketAddr::new(local_ip, port);
    let add =
        |lease: u32| gateway.add_port(PortMappingProtocol::TCP, port, local, lease, DESCRIPTION);
    let result = match add(LEASE.as_secs() as u32).await {
        // Some routers only take permanent rules; renewing is harmless.
        Err(AddPortError::OnlyPermanentLeasesSupported) => add(0).await,
        other => other,
    };
    let already_mapped = match result {
        Ok(()) => false,
        // Most often a manual forward for the same port; it may well point
        // at us, so this is not treated as a failure.
        Err(AddPortError::PortInUse) => true,
        Err(e) => return Err(format!("router refused the mapping ({e})")),
    };
    let external_ip = gateway.get_external_ip().await.ok();
    Ok(PortMapping {
        port,
        external_ip,
        already_mapped,
    })
}

/// Removes our mapping for `port`, ignoring errors (the lease expires
/// anyway).
pub async fn unmap(port: u16) {
    let Ok(gateway) = find_gateway().await else {
        return;
    };
    let _ = gateway.remove_port(PortMappingProtocol::TCP, port).await;
}

/// The address of the interface that routes to `peer`. Connecting a UDP
/// socket sends nothing; it only asks the kernel for a route.
fn local_ip_towards(peer: SocketAddr) -> std::io::Result<IpAddr> {
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.connect(peer)?;
    Ok(socket.local_addr()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_gateway() {
        let table = "Iface\tDestination\tGateway \tFlags\n\
                     wlan0\t00000000\t0100A8C0\t0003\n\
                     wlan0\t0000A8C0\t00000000\t0001\n";
        assert_eq!(
            parse_default_gateway(table),
            Some(Ipv4Addr::new(192, 168, 0, 1))
        );
        assert_eq!(parse_default_gateway("Iface Destination Gateway\n"), None);
    }

    #[test]
    fn local_ip_for_loopback_is_loopback() {
        let ip = local_ip_towards("127.0.0.1:1900".parse().unwrap()).unwrap();
        assert!(ip.is_loopback());
    }
}

/// What the automatic port mapping currently does, as reported to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortMapStatus {
    Disabled,
    Trying,
    Mapped(PortMapping),
    Failed(String),
}

impl PortMapping {
    /// The router's own WAN address is private: another NAT (usually the
    /// ISP's modem) sits in front of it, and UPnP only opens this router.
    pub fn behind_another_nat(&self) -> bool {
        match self.external_ip {
            Some(IpAddr::V4(ip)) => {
                let [a, b, ..] = ip.octets();
                ip.is_private() || (a == 100 && (64..128).contains(&b))
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod nat_tests {
    use super::*;

    #[test]
    fn detects_double_nat() {
        let m = |ip: [u8; 4]| PortMapping {
            port: 2234,
            external_ip: Some(IpAddr::from(ip)),
            already_mapped: false,
        };
        assert!(m([192, 168, 100, 2]).behind_another_nat());
        assert!(m([100, 64, 1, 1]).behind_another_nat());
        assert!(!m([94, 21, 69, 106]).behind_another_nat());
    }
}
