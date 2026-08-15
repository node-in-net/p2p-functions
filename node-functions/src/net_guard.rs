//! Where a peer's relayed traffic is allowed to go. Sharing an internet
//! connection is not the same as opening the LAN, but to a proxy both are just
//! a host and a port.

use std::net::{IpAddr, SocketAddr};

/// True when `addr` is ours or shares a subnet with one of our interfaces.
/// Ranges cannot answer this: on IPv6 a router and a web server both sit in
/// 2000::/3, and our own routable address looks public too.
fn is_on_our_network(addr: &IpAddr) -> bool {
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};
    let Ok(ifaces) = NetworkInterface::show() else {
        return false;
    };
    ifaces
        .iter()
        .flat_map(|i| i.addr.iter())
        .any(|a| match (a, addr) {
            (network_interface::Addr::V4(v4), IpAddr::V4(t)) => {
                v4.ip == *t
                    || v4.netmask.is_some_and(|m| {
                        let (ip, m, t) = (u32::from(v4.ip), u32::from(m), u32::from(*t));
                        ip & m == t & m
                    })
            }
            (network_interface::Addr::V6(v6), IpAddr::V6(t)) => {
                v6.ip == *t
                    || v6.netmask.is_some_and(|m| {
                        let (ip, m, t) = (v6.ip.octets(), m.octets(), t.octets());
                        (0..16).all(|i| ip[i] & m[i] == t[i] & m[i])
                    })
            }
            _ => false,
        })
}

/// True when `addr` is reachable only from this machine or its local network.
pub fn is_local_target(addr: &IpAddr) -> bool {
    if is_on_our_network(addr) {
        return true;
    }
    is_in_local_range(addr)
}

/// The address ranges that are local no matter what the interfaces say.
fn is_in_local_range(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local() // 169.254/16, incl. the cloud metadata address
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_unspecified()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64/10 carrier-grade NAT
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_in_local_range(&IpAddr::V4(mapped));
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || (first & 0xfe00) == 0xfc00 // fc00::/7  unique local
                || (first & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

/// Resolves `host:port` and keeps the addresses a peer may be connected to.
///
/// Filters the resolved address, not the name — any name can point at
/// `192.168.1.1`. Connect to what this returns; a second lookup can differ.
pub async fn resolve_allowed(
    host: &str,
    port: u16,
    allow_local: bool,
) -> Result<Vec<SocketAddr>, String> {
    let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("Could not resolve {host}: {e}"))?
        .collect();

    if resolved.is_empty() {
        return Err(format!("{host} resolved to no address"));
    }
    if allow_local {
        return Ok(resolved);
    }

    let allowed: Vec<SocketAddr> = resolved
        .into_iter()
        .filter(|a| !is_local_target(&a.ip()))
        .collect();

    if allowed.is_empty() {
        return Err(format!(
            "{host} is on a local network; enable local network access to allow it"
        ));
    }
    Ok(allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(s: &str) -> bool {
        is_local_target(&s.parse().unwrap())
    }

    #[test]
    fn blocks_loopback_and_private_ranges() {
        for a in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.1",
            "0.0.0.0",
            "100.64.0.1",
        ] {
            assert!(local(a), "{a} should be treated as local");
        }
    }

    #[test]
    fn blocks_the_cloud_metadata_address() {
        assert!(local("169.254.169.254"));
    }

    #[test]
    fn allows_public_addresses() {
        for a in ["8.8.8.8", "1.1.1.1", "172.32.0.1", "93.184.216.34"] {
            assert!(!local(a), "{a} should be reachable");
        }
    }

    #[test]
    fn blocks_ipv6_loopback_and_local_ranges() {
        for a in ["::1", "fe80::1", "fc00::1", "fd12:3456::1", "::"] {
            assert!(local(a), "{a} should be treated as local");
        }
        assert!(!local("2606:4700:4700::1111"));
    }

    #[test]
    fn sees_through_ipv4_mapped_ipv6() {
        assert!(local("::ffff:192.168.1.1"));
        assert!(local("::ffff:127.0.0.1"));
        assert!(!local("::ffff:8.8.8.8"));
    }

    #[tokio::test]
    async fn rejects_a_name_that_resolves_into_the_lan() {
        let err = resolve_allowed("localhost", 80, false).await;
        assert!(err.is_err(), "localhost must not be reachable by default");
    }

    /// Range checks cannot see this: the machine's own addresses are ordinary
    /// public ones, and a service bound to 0.0.0.0 answers on them.
    #[test]
    fn blocks_this_machines_own_addresses() {
        use network_interface::{NetworkInterface, NetworkInterfaceConfig};
        let mine: Vec<IpAddr> = NetworkInterface::show()
            .unwrap_or_default()
            .iter()
            .flat_map(|i| i.addr.iter())
            .map(|a| a.ip())
            .collect();
        assert!(!mine.is_empty(), "no interfaces to test against");
        for ip in mine {
            assert!(is_local_target(&ip), "{ip} is our own address");
        }
    }

    #[tokio::test]
    async fn allows_the_same_name_once_local_access_is_on() {
        let ok = resolve_allowed("localhost", 80, true).await;
        assert!(ok.is_ok());
    }
}
