//! The pond's `_pond._tcp` instance through mdns-sd, a responder inside the pond's own process.
//! It is the advertiser off macOS: on Linux it answers beside avahi (the Jetson resolves its own
//! pond). It builds on every OS so its tests run on the Mac.

use std::sync::Mutex;

use async_trait::async_trait;
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};
use pond_core::network::domain::pond_advert::{AdvertiseError, PondAdvert, POND_SERVICE_TYPE};
use pond_core::network::ports::service_advertiser::ServiceAdvertiser;

pub const RESPONDER: &str = "mdns-sd";

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

fn service_type() -> String {
    format!("{POND_SERVICE_TYPE}.local.")
}

fn service_info(advert: &PondAdvert) -> Result<ServiceInfo, AdvertiseError> {
    let txt = advert.txt();
    let properties: Vec<(&str, &str)> = txt.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let info = ServiceInfo::new(
        &service_type(),
        &advert.instance(),
        &format!("{}.local.", advert.host),
        "", // addresses filled in by enable_addr_auto() below
        advert.port,
        properties.as_slice(),
    )
    .map_err(|e| AdvertiseError::Refused(e.to_string()))?;
    // mdns-sd won't fill addresses for an empty host; without this phones get no resolvable IP.
    Ok(info.enable_addr_auto())
}

fn start_daemon() -> Result<ServiceDaemon, AdvertiseError> {
    let daemon = ServiceDaemon::new().map_err(|e| AdvertiseError::Unavailable(e.to_string()))?;
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
    Ok(daemon)
}

struct Registered {
    full_name: String,
    instance: String,
    port: u16,
}

#[derive(Default)]
struct State {
    daemon: Option<ServiceDaemon>,
    registered: Option<Registered>,
}

impl State {
    fn unregister(&mut self) {
        if let (Some(daemon), Some(registered)) = (&self.daemon, self.registered.take()) {
            if let Err(e) = daemon.unregister(&registered.full_name) {
                tracing::warn!("mDNS deregister failed: {e}");
            }
        }
    }
}

#[derive(Default)]
pub struct MdnsSdAdvertiser {
    state: Mutex<State>,
}

impl MdnsSdAdvertiser {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait]
impl ServiceAdvertiser for MdnsSdAdvertiser {
    async fn publish(&self, advert: &PondAdvert) -> Result<(), AdvertiseError> {
        let info = service_info(advert)?;
        let instance = advert.instance();
        let mut state = self.state();
        // A new name or port retires the old instance; the same one is replaced in place.
        if state
            .registered
            .as_ref()
            .is_some_and(|r| r.instance != instance || r.port != advert.port)
        {
            state.unregister();
        }
        let daemon = match state.daemon.take() {
            Some(daemon) => daemon,
            None => start_daemon()?,
        };
        let full_name = info.get_fullname().to_string();
        let registered = daemon.register(info);
        state.daemon = Some(daemon);
        registered.map_err(|e| AdvertiseError::Unavailable(e.to_string()))?;
        state.registered = Some(Registered {
            full_name,
            instance,
            port: advert.port,
        });
        Ok(())
    }

    async fn withdraw(&self) {
        let mut state = self.state();
        state.unregister();
        if let Some(daemon) = state.daemon.take() {
            if let Err(e) = daemon.shutdown() {
                tracing::debug!("mDNS daemon shutdown: {e}");
            }
        }
    }

    fn responder(&self) -> &'static str {
        RESPONDER
    }
}

impl Drop for MdnsSdAdvertiser {
    fn drop(&mut self) {
        self.state().unregister();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::network::domain::pond_advert::SetupAdvert;

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
    fn builds_one_name_per_prefix_and_index() {
        assert_eq!(
            tunnel_interface_names().len(),
            TUNNEL_PREFIXES.len() * TUNNEL_INDEX_LIMIT
        );
    }

    #[test]
    fn the_service_carries_the_txt_contract_under_the_pond_type() {
        let advert =
            PondAdvert::new("nano", 4443, "0.1.0", "sha256/x").with_setup(Some(SetupAdvert {
                id: "7f3a".into(),
                unclaimed: true,
            }));
        let info = service_info(&advert).unwrap();
        assert_eq!(info.get_type(), "_pond._tcp.local.");
        assert_eq!(info.get_fullname(), "Pond Hub @ nano._pond._tcp.local.");
        assert_eq!(info.get_hostname(), "nano.local.");
        assert_eq!(info.get_port(), 4443);
        for (key, value) in advert.txt() {
            assert_eq!(
                info.get_property_val_str(key),
                Some(value.as_str()),
                "{key}"
            );
        }
        assert_eq!(info.get_properties().len(), 5);
    }

    /// Run with a VPN up: `cargo test -p pond-connect -- --ignored tunnels_this_host`.
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
