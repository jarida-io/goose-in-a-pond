//! Which of the host's addresses put the pond on the household's network, from names and
//! addresses alone, so the pairing boundary and the advertising decision judge them one way.

use std::net::{IpAddr, Ipv4Addr};

/// Overlay VPNs that present as Ethernet (ZeroTier, NetBird, Hamachi, Nebula) are not
/// point-to-point, so only their names give them away; container bridges are not the house.
pub const NON_LAN_INTERFACE_PREFIXES: &[&str] = &[
    "tailscale",
    "utun",
    "tun",
    "tap",
    "wg",
    "wt",
    "zt",
    "feth",
    "ham",
    "nebula",
    "ipsec",
    "gif",
    "stf",
    "ppp",
    "docker",
    "veth",
    "virbr",
];

/// Whether an address belongs to Tailscale's IPv4 or IPv6 allocations.
pub fn is_tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => u32::from(v) & 0xffc0_0000 == 0x6440_0000,
        IpAddr::V6(v) => v.to_ipv4_mapped().map_or_else(
            || v.octets()[..6] == [0xfd, 0x7a, 0x11, 0x5c, 0xa1, 0xe0],
            |v| is_tailnet(v.into()),
        ),
    }
}

/// `fe80::/16` exactly, as the interface walk has always judged it.
fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_link_local(),
        IpAddr::V6(v) => v.octets()[..2] == [0xfe, 0x80],
    }
}

/// An address on a directly attached household network: not loopback, link-local, a tailnet,
/// a tunnel or a container bridge.
pub fn is_lan_address(interface_name: &str, ip: IpAddr) -> bool {
    !ip.is_loopback()
        && !is_link_local(ip)
        && !is_tailnet(ip)
        && !NON_LAN_INTERFACE_PREFIXES
            .iter()
            .any(|prefix| interface_name.starts_with(prefix))
}

/// The advertising policy's "usable network": an IPv4 address on a household LAN interface.
pub fn has_usable_network<'a>(addresses: impl IntoIterator<Item = (&'a str, Ipv4Addr)>) -> bool {
    addresses
        .into_iter()
        .any(|(name, ip)| is_lan_address(name, IpAddr::V4(ip)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(text: &str) -> Ipv4Addr {
        text.parse().unwrap()
    }

    #[test]
    fn only_a_household_lan_address_counts() {
        assert!(is_lan_address("en0", "192.168.1.2".parse().unwrap()));
        assert!(is_lan_address("eth0", "10.0.0.7".parse().unwrap()));
        for (name, ip) in [
            ("lo0", "127.0.0.1"),
            ("en0", "169.254.10.2"),
            ("en0", "fe80::1"),
            ("en0", "100.64.0.2"),
            ("en0", "fd7a:115c:a1e0::1"),
            ("utun3", "10.8.0.2"),
            ("wg0", "10.9.0.2"),
            ("docker0", "172.17.0.1"),
            ("zt5u4y6j", "192.168.191.2"),
        ] {
            assert!(!is_lan_address(name, ip.parse().unwrap()), "{name} {ip}");
        }
    }

    #[test]
    fn a_usable_network_is_an_ipv4_address_on_a_lan_interface() {
        assert!(!has_usable_network([]));
        assert!(!has_usable_network([
            ("lo0", v4("127.0.0.1")),
            ("en0", v4("169.254.3.4")),
            ("utun0", v4("10.8.0.2")),
            ("docker0", v4("172.17.0.1")),
        ]));
        assert!(has_usable_network([
            ("lo0", v4("127.0.0.1")),
            ("en1", v4("192.168.1.40")),
        ]));
    }
}
