//! mDNS service advertisement for LAN discovery.
//!
//! Registers a `_pond._tcp.local.` service so phones on the same network can
//! find this hub without manual IP entry.  The [`MdnsHandle`] keeps the
//! background `ServiceDaemon` alive; dropping it deregisters the service.

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
    "utun", "ipsec", "ppp", // Linux: OpenVPN and IPIP, WireGuard, bridged tap
    "tun", "wg", "tap",
];

/// How many indices of each prefix to exclude, e.g. `utun0` through `utun15`.
///
/// macOS numbers tunnels from zero and a laptop with a VPN, Handoff and an
/// iCloud Private Relay session runs four or five; sixteen is slack for a host
/// that runs several at once.
const TUNNEL_INDEX_LIMIT: usize = 16;

/// The interface names to exclude, built rather than enumerated.
///
/// **This does not ask the OS what exists, and that is the point.** The first
/// version of this enumerated live interfaces with `if-addrs` and shipped doing
/// nothing at all, because mdns-sd depends on `if-addrs` 0.13 with the
/// `link-local` feature while a fresh `if-addrs = "0.15"` here resolved to a
/// second copy without it. Two versions, no feature unification, and the
/// addresses on a tunnel are exactly the `fe80::` link-locals that the feature
/// governs -- so the list came back empty, every unit test passed against
/// hand-typed names, and the error kept printing.
///
/// `IfKind::Name` rules are matched against whatever interfaces exist each time
/// the daemon re-applies its selections, so a rule naming an interface that is
/// absent is simply inert. Building the name space instead of sampling it also
/// covers the common case the enumerating version could never have handled: a
/// VPN dialled AFTER the server started.
fn tunnel_interface_names() -> Vec<String> {
    TUNNEL_PREFIXES
        .iter()
        .flat_map(|prefix| (0..TUNNEL_INDEX_LIMIT).map(move |i| format!("{prefix}{i}")))
        .collect()
}

/// Holds the running mDNS daemon. Drop to deregister.
pub struct MdnsHandle {
    daemon: ServiceDaemon,
    full_name: String,
}

impl Drop for MdnsHandle {
    fn drop(&mut self) {
        if let Err(e) = self.daemon.unregister(&self.full_name) {
            tracing::warn!("mDNS deregister failed: {e}");
        }
    }
}

/// Advertise `_pond._tcp.local.` on `port` using `hostname` as the instance label.
///
/// Returns [`None`] (with a warning log) rather than propagating the error, so
/// a missing mDNS stack never prevents the server from starting.
pub fn advertise(hostname: &str, port: u16, version: &str) -> Result<MdnsHandle> {
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

    let service_type = "_pond._tcp.local.";
    let instance_name = format!("Pond Hub @ {hostname}");
    let host_fqdn = format!("{hostname}.local.");

    let mut properties = std::collections::HashMap::new();
    properties.insert("v".to_string(), version.to_string());

    let service = ServiceInfo::new(
        service_type,
        &instance_name,
        &host_fqdn,
        "", // addresses filled in by enable_addr_auto() below
        port,
        Some(properties),
    )?
    // mdns-sd does NOT auto-populate interface addresses from an empty host —
    // without this the service advertises with no resolvable IP and phones
    // silently fail to connect. enable_addr_auto() makes the daemon announce
    // (and keep updated) every reachable interface address.
    .enable_addr_auto();

    let full_name = service.get_fullname().to_string();
    daemon.register(service)?;

    tracing::info!(
        hostname,
        port,
        "_pond._tcp.local. registered — phones on the same LAN can now discover this hub"
    );

    Ok(MdnsHandle { daemon, full_name })
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

    /// AWDL is how Apple peer-to-peer does mDNS, and llw is its low-latency
    /// sibling. Neither has been observed failing here, and excluding them
    /// would remove a working transport to cure noise it is not making.
    #[test]
    fn excludes_no_real_lan_interface_and_no_awdl() {
        let names = tunnel_interface_names();
        for real in [
            // macOS
            "en0", "en1", "bridge0", "awdl0", "llw0", "lo0",
            // Linux, including the predictable-naming and container forms the
            // Jetson will actually have. `tap`/`tun` are prefixes of nothing
            // here, which is what makes them safe to list.
            "eth0", "wlan0", "enp3s0", "wlp2s0", "docker0", "virbr0", "veth1a2b", "lo",
        ] {
            assert!(
                !names.iter().any(|n| n == real),
                "{real} would be excluded from mDNS, and it can carry LAN discovery"
            );
        }
    }

    #[test]
    fn builds_one_name_per_prefix_and_index() {
        assert_eq!(
            tunnel_interface_names().len(),
            TUNNEL_PREFIXES.len() * TUNNEL_INDEX_LIMIT
        );
    }

    /// The test the first version of this fix did not have, and the reason it
    /// shipped doing nothing.
    ///
    /// That version enumerated live interfaces through a second, feature-poorer
    /// copy of `if-addrs` than the one mdns-sd uses, so the list came back
    /// EMPTY on a host with four tunnels up. Every unit test passed, because
    /// every unit test fed the predicate names typed by hand. Nothing compared
    /// the exclusion list against the machine.
    ///
    /// Ignored by default because it asserts about the host: it needs a tunnel
    /// interface up to mean anything, and it is BSD/macOS-shaped. Run with
    /// `cargo test -p pond-server --bin pond-server -- --ignored` on a machine
    /// with a VPN connected.
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
