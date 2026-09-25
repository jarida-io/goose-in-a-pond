//! Network metadata and the direct-LAN boundary for first pairing.

use axum::{extract::ConnectInfo, http::StatusCode, Extension, Json};
use if_addrs::{IfAddr, Interface};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::{IpAddr, SocketAddr};

/// Public transport metadata, supplied by the process that owns the listeners.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompanionTransport {
    /// Actual HTTPS listener port, including fallback selection.
    pub https_port: u16,
    /// SHA-256 of DER SubjectPublicKeyInfo, in `sha256/<base64>` format.
    pub tls_spki_sha256: String,
}

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

/// Exclude tunnels and container-only networks from the home LAN boundary.
pub fn is_lan_interface(interface: &Interface) -> bool {
    !interface.is_loopback()
        && !interface.is_link_local()
        && !is_tailnet(interface.ip())
        && ![
            "tailscale",
            "utun",
            "tun",
            "tap",
            "wg",
            "ipsec",
            "ppp",
            "docker",
            "veth",
            "virbr",
        ]
        .iter()
        .any(|prefix| interface.name.starts_with(prefix))
}

/// Read current interface addresses; callers must fail closed on an error.
pub fn interfaces() -> std::io::Result<Vec<Interface>> {
    let mut addresses = if_addrs::get_if_addrs()?;
    #[cfg(unix)]
    {
        // if-addrs omits interface flags. Read them to reject renamed point-to-point
        // tunnels and down interfaces, independently of the defensive name filter.
        let allowed = attached_interface_names()?;
        addresses.retain(|i| is_tailnet(i.ip()) || allowed.contains(&i.name));
    }
    #[cfg(not(unix))]
    addresses.clear(); // Pairing classification is not implemented on this platform.
    Ok(addresses)
}

#[cfg(unix)]
fn attached_interface_names() -> std::io::Result<std::collections::HashSet<String>> {
    let mut head = std::ptr::null_mut();
    // SAFETY: getifaddrs initializes a linked list owned by libc on success.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    struct List(*mut libc::ifaddrs);
    impl Drop for List {
        fn drop(&mut self) {
            // SAFETY: this pointer came from the successful getifaddrs call and is freed once.
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
    let list = List(head);
    let mut current = list.0;
    let mut names = std::collections::HashSet::new();
    while !current.is_null() {
        // SAFETY: the list remains allocated until its guard is dropped.
        let interface = unsafe { &*current };
        let flags = interface.ifa_flags as libc::c_int;
        if flags & libc::IFF_UP != 0
            && flags & libc::IFF_POINTOPOINT == 0
            && !interface.ifa_name.is_null()
        {
            // SAFETY: getifaddrs guarantees a nul-terminated interface name.
            if let Ok(name) = unsafe { std::ffi::CStr::from_ptr(interface.ifa_name) }.to_str() {
                names.insert(name.to_owned());
            }
        }
        current = interface.ifa_next;
    }
    Ok(names)
}

/// Match the actual TCP source to a directly attached LAN, never forwarded headers.
pub fn is_lan_peer(peer: IpAddr, interfaces: &[Interface]) -> bool {
    let peer = match peer {
        IpAddr::V6(v) => v.to_ipv4_mapped().map_or(IpAddr::V6(v), IpAddr::V4),
        peer => peer,
    };
    if peer.is_loopback() {
        return true;
    }
    if peer.is_unspecified() || peer.is_multicast() || is_tailnet(peer) {
        return false;
    }
    interfaces
        .iter()
        .filter(|i| is_lan_interface(i))
        .any(|i| match (&i.addr, peer) {
            (IfAddr::V4(local), IpAddr::V4(peer)) => {
                let mask = u32::from(local.netmask);
                mask != 0
                    && (u32::from(peer) & mask) == (u32::from(local.ip) & mask)
                    && Some(peer) != local.broadcast
            }
            (IfAddr::V6(local), IpAddr::V6(peer)) => {
                let mask = u128::from(local.netmask);
                mask != 0 && (u128::from(peer) & mask) == (u128::from(local.ip) & mask)
            }
            _ => false,
        })
}

/// Pairing is local even when a caller knows a valid unconsumed code.
pub fn require_lan(peer: Option<ConnectInfo<SocketAddr>>) -> Result<(), (StatusCode, Json<Value>)> {
    let allowed = peer.as_ref().is_some_and(|ConnectInfo(peer)| {
        if peer.ip().is_loopback() {
            return true;
        }
        match interfaces() {
            Ok(interfaces) => is_lan_peer(peer.ip(), &interfaces),
            Err(error) => {
                tracing::warn!(%error, "cannot classify pairing peer interfaces");
                false
            }
        }
    });
    if allowed {
        return Ok(());
    }
    tracing::warn!(peer = ?peer.map(|p| p.0.ip()), "pairing rejected outside the direct LAN");
    Err((
        StatusCode::FORBIDDEN,
        Json(json!({"error": "pairing_requires_lan"})),
    ))
}

/// Additive transport fields for old clients of `/system/info`.
pub fn transport_fields(
    transport: Option<Extension<CompanionTransport>>,
) -> (Option<u16>, Option<String>) {
    match transport {
        Some(Extension(t)) => (Some(t.https_port), Some(t.tls_spki_sha256)),
        None => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lan(name: &str, ip: &str) -> Interface {
        Interface {
            name: name.into(),
            index: None,
            #[cfg(windows)]
            adapter_name: name.into(),
            addr: IfAddr::V4(if_addrs::Ifv4Addr {
                ip: ip.parse().unwrap(),
                netmask: "255.255.255.0".parse().unwrap(),
                prefixlen: 24,
                broadcast: None,
            }),
        }
    }
    #[test]
    fn only_the_attached_lan_and_loopback_can_pair() {
        let interfaces = [lan("en0", "192.168.1.2"), lan("tailscale0", "100.64.0.2")];
        for ip in ["127.0.0.1", "::1", "192.168.1.3", "::ffff:192.168.1.3"] {
            assert!(is_lan_peer(ip.parse().unwrap(), &interfaces), "{ip}");
        }
        for ip in [
            "192.168.2.3",
            "10.0.0.3",
            "100.64.0.3",
            "fd7a:115c:a1e0::1",
            "8.8.8.8",
        ] {
            assert!(!is_lan_peer(ip.parse().unwrap(), &interfaces), "{ip}");
        }
        assert!(!is_lan_peer(
            "192.168.1.3".parse().unwrap(),
            &[lan("wg0", "192.168.1.2")]
        ));
        assert_eq!(require_lan(None).unwrap_err().0, StatusCode::FORBIDDEN);
    }
}
