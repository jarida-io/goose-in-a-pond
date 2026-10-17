//! Advertises `_pond._tcp.local.` so phones on the LAN find this hub without an IP.

use anyhow::Result;
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};

/// Interface-name prefixes that cannot carry LAN service discovery.
///
/// A VPN or point-to-point tunnel has no LAN segment to multicast onto. macOS
/// returns ENOBUFS for an IPv6 multicast send on one, and mdns-sd logs that at
/// `error!` for every announcement on a repeating timer:
///
/// ```text
/// ERROR mdns_sd::service_daemon: Failed to send to [ff02::fb%15]:5353 via
/// Interface { name: "utun0", .. }: No buffer space available (os error 55)
/// ```
///
/// That level is inside the library, so the only way to quiet it is to stop
/// handing the daemon the interface. Discovery is unaffected -- phones reach
/// this hub over the real LAN interface, never over the operator's VPN.
///
/// Deliberately does NOT list `awdl`/`llw`: AWDL is a genuine mDNS transport
/// for Apple peer-to-peer, and it has not been observed failing here.
///
/// The symptom above was observed on macOS, whose tunnels are `utun`/`ipsec`/
/// `ppp`. The Linux names are listed too, because the Jetson is the primary
/// deployment target and a hub there is at least as likely to sit behind
/// WireGuard as a developer's laptop. Whether Linux reproduces this exact error
/// is untested -- but a rule naming an absent interface is inert, so listing
/// them costs nothing and not listing them would leave the real target
/// uncovered.
///
/// No real LAN interface is shadowed by these: Linux names its wired and
/// wireless interfaces `eth*`, `en*` and `wl*`, and Docker and libvirt use
/// `veth*`, `docker*` and `virbr*`.
const TUNNEL_PREFIXES: &[&str] = &[
    // macOS / BSD
    "utun",
    "ipsec",
    "ppp", // Linux: OpenVPN and IPIP, WireGuard, bridged tap
    "tun",
    "wg",
    "tap",
    "tailscale",
];

/// Indices excluded per prefix (`utun0`..`utun15`); a busy Mac runs four or five tunnels.
const TUNNEL_INDEX_LIMIT: usize = 16;

/// Exclusion names, built rather than read from the OS: absent names are inert, a later VPN
/// is covered, and a second `if-addrs` copy (no `link-local`) once returned none.
fn tunnel_interface_names() -> Vec<String> {
    TUNNEL_PREFIXES
        .iter()
        .flat_map(|prefix| (0..TUNNEL_INDEX_LIMIT).map(move |i| format!("{prefix}{i}")))
        .collect()
}

/// What the TXT record says about setup: the pond's id, and whether its card is still accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupAdvert {
    /// The discriminator printed on the card, so a phone connects to the pond it scanned.
    pub id: String,
    pub unclaimed: bool,
}

/// Holds the running mDNS daemon. Drop to deregister.
pub struct MdnsHandle {
    daemon: ServiceDaemon,
    full_name: String,
    instance_name: String,
    host_fqdn: String,
    port: u16,
    version: String,
    pin: String,
}

impl MdnsHandle {
    /// Re-announces the same instance with a new setup state; mdns-sd replaces the record in place.
    pub fn set_setup(&self, setup: Option<&SetupAdvert>) -> Result<()> {
        let service = service_info(
            &self.instance_name,
            &self.host_fqdn,
            self.port,
            txt_properties(&self.version, &self.pin, setup),
        )?;
        self.daemon.register(service)?;
        Ok(())
    }
}

impl Drop for MdnsHandle {
    fn drop(&mut self) {
        if let Err(e) = self.daemon.unregister(&self.full_name) {
            tracing::warn!("mDNS deregister failed: {e}");
        }
    }
}

const SERVICE_TYPE: &str = "_pond._tcp.local.";

/// The TXT record. `setup=1` only while the card is accepted, so a phone holding one looks only
/// at ponds that will take it.
fn txt_properties(
    version: &str,
    pin: &str,
    setup: Option<&SetupAdvert>,
) -> std::collections::HashMap<String, String> {
    let mut properties = std::collections::HashMap::new();
    properties.insert("v".to_string(), version.to_string());
    properties.insert("scheme".to_string(), "https".to_string());
    // Public, and only a convenience: mDNS is spoofable, so clients must confirm the pin.
    properties.insert("pin".to_string(), pin.to_string());
    if let Some(setup) = setup {
        properties.insert("id".to_string(), setup.id.clone());
        if setup.unclaimed {
            properties.insert("setup".to_string(), "1".to_string());
        }
    }
    properties
}

fn service_info(
    instance_name: &str,
    host_fqdn: &str,
    port: u16,
    properties: std::collections::HashMap<String, String>,
) -> Result<ServiceInfo> {
    Ok(ServiceInfo::new(
        SERVICE_TYPE,
        instance_name,
        host_fqdn,
        "", // addresses filled in by enable_addr_auto() below
        port,
        Some(properties),
    )?
    // mdns-sd won't fill addresses for an empty host; without this phones get no resolvable IP.
    .enable_addr_auto())
}

/// Advertise `_pond._tcp.local.` on `port` using `hostname` as the instance label.
pub fn advertise(
    hostname: &str,
    port: u16,
    version: &str,
    pin: &str,
    setup: Option<&SetupAdvert>,
) -> Result<MdnsHandle> {
    let daemon = ServiceDaemon::new()?;

    let tunnels = tunnel_interface_names();
    let kinds: Vec<IfKind> = tunnels.iter().cloned().map(IfKind::Name).collect();
    match daemon.disable_interface(kinds) {
        Ok(()) => tracing::info!(
            prefixes = ?TUNNEL_PREFIXES,
            count = tunnels.len(),
            "mDNS: excluding tunnel interfaces, which cannot carry LAN discovery and log an error per announcement"
        ),
        Err(e) => tracing::warn!(
            "mDNS: could not exclude tunnel interfaces ({e}); expect send errors in the log"
        ),
    }

    let instance_name = format!("Pond Hub @ {hostname}");
    let host_fqdn = format!("{hostname}.local.");
    let service = service_info(
        &instance_name,
        &host_fqdn,
        port,
        txt_properties(version, pin, setup),
    )?;

    let full_name = service.get_fullname().to_string();
    daemon.register(service)?;

    tracing::info!(
        hostname,
        port,
        setup_open = setup.is_some_and(|s| s.unclaimed),
        "_pond._tcp.local. registered — phones on the same LAN can now discover this hub"
    );

    Ok(MdnsHandle {
        daemon,
        full_name,
        instance_name,
        host_fqdn,
        port,
        version: version.to_string(),
        pin: pin.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_the_interface_that_reported_the_bug() {
        let names = tunnel_interface_names();
        assert!(names.iter().any(|n| n == "utun0"));
        assert!(names.iter().any(|n| n == "utun3"));
        assert!(names.iter().any(|n| n == "ipsec0"));
        assert!(names.iter().any(|n| n == "ppp0"));
    }

    #[test]
    fn excludes_no_real_lan_interface_and_no_awdl() {
        let names = tunnel_interface_names();
        for real in [
            // macOS
            "en0", "en1", "bridge0", "awdl0", "llw0", "lo0",
            // Linux, incl. predictable and container names; `tap`/`tun` prefix none of them.
            "eth0", "wlan0", "enp3s0", "wlp2s0", "docker0", "virbr0", "veth1a2b", "lo",
        ] {
            assert!(
                !names.iter().any(|n| n == real),
                "{real} would be excluded from mDNS, and it can carry LAN discovery"
            );
        }
    }

    #[test]
    fn the_txt_record_names_the_pond_and_opens_setup_only_while_unclaimed() {
        let base = txt_properties("1.0", "sha256/x", None);
        assert_eq!(base.get("id"), None);
        assert_eq!(base.get("setup"), None);
        assert_eq!(base.get("pin").map(String::as_str), Some("sha256/x"));

        let open = SetupAdvert {
            id: "7f3a".into(),
            unclaimed: true,
        };
        let txt = txt_properties("1.0", "sha256/x", Some(&open));
        assert_eq!(txt.get("id").map(String::as_str), Some("7f3a"));
        assert_eq!(txt.get("setup").map(String::as_str), Some("1"));

        let claimed = SetupAdvert {
            unclaimed: false,
            ..open
        };
        let txt = txt_properties("1.0", "sha256/x", Some(&claimed));
        assert_eq!(txt.get("id").map(String::as_str), Some("7f3a"));
        assert_eq!(
            txt.get("setup"),
            None,
            "a claimed pond must not invite a card"
        );
    }

    #[test]
    fn builds_one_name_per_prefix_and_index() {
        assert_eq!(
            tunnel_interface_names().len(),
            TUNNEL_PREFIXES.len() * TUNNEL_INDEX_LIMIT
        );
    }

    /// Run with a VPN up: `cargo test -p pond-server --bin pond-server -- --ignored`.
    #[test]
    #[ignore = "asserts about the host's live interfaces; needs a tunnel up"]
    fn the_exclusion_list_covers_the_tunnels_this_host_actually_has() {
        let out = std::process::Command::new("ifconfig")
            .arg("-l")
            .output()
            .expect("ifconfig -l");
        let listed = String::from_utf8_lossy(&out.stdout);
        let host_tunnels: Vec<&str> = listed
            .split_whitespace()
            .filter(|n| TUNNEL_PREFIXES.iter().any(|p| n.starts_with(p)))
            .collect();

        assert!(
            !host_tunnels.is_empty(),
            "no tunnel interface is up, so this test proves nothing -- connect a VPN"
        );

        let names = tunnel_interface_names();
        for t in &host_tunnels {
            assert!(
                names.iter().any(|n| n == t),
                "{t} is up on this host and is NOT in the exclusion list, so mDNS will \
                 multicast on it and log an error per announcement. Host tunnels: \
                 {host_tunnels:?}"
            );
        }
    }
}
