//! What the station asks of NetworkManager, and how NetworkManager's answers read in the spec's
//! codes. The calls sit behind a trait so the station's decisions are tested without a bus.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::Ipv4Addr;

use async_trait::async_trait;
use futures::stream::BoxStream;
use pond_core::network::domain::capability::Availability;
use pond_core::network::domain::wifi::{
    WifiCapabilities, WifiJoinError, WifiNetwork, WifiScanError, WifiSecurity, MAX_SIGNAL,
};

pub(super) const DEVICE_TYPE_WIFI: u32 = 2;

pub(super) const STATE_DISCONNECTED: u32 = 30;
pub(super) const STATE_PREPARE: u32 = 40;
pub(super) const STATE_SECONDARIES: u32 = 90;
pub(super) const STATE_ACTIVATED: u32 = 100;
pub(super) const STATE_FAILED: u32 = 120;

pub(super) const AP_FLAGS_PRIVACY: u32 = 0x1;
const SEC_KEY_MGMT_PSK: u32 = 0x100;
const SEC_KEY_MGMT_802_1X: u32 = 0x200;
const SEC_KEY_MGMT_SAE: u32 = 0x400;
const SEC_KEY_MGMT_OWE: u32 = 0x800;
const SEC_KEY_MGMT_OWE_TM: u32 = 0x1000;
const SEC_KEY_MGMT_EAP_SUITE_B_192: u32 = 0x2000;

const PERMISSION_MODIFY_SYSTEM: &str = "org.freedesktop.NetworkManager.settings.modify.system";
const PERMISSION_NETWORK_CONTROL: &str = "org.freedesktop.NetworkManager.network-control";

pub(super) const NO_GRANT: &str = "this pond may not change its Wi-Fi: the installer's network grant is missing (bash scripts/giap.sh wifi-grant)";
pub(super) const NO_DEVICE: &str = "this host has no Wi-Fi adapter";
pub(super) const HARDWARE_OFF: &str = "Wi-Fi is switched off by a hardware or rfkill switch";

/// Why NetworkManager could not be asked, or what it answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NmError {
    /// No system bus, or NetworkManager is not on it; says which.
    Unreachable(String),
    /// NetworkManager refused, by D-Bus error name. Its message is never kept: for a join it may
    /// describe the passphrase.
    Refused(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WifiDevice {
    pub path: String,
    pub interface: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DeviceLookup {
    Found(WifiDevice),
    /// A Wi-Fi adapter exists, but NetworkManager leaves it to something else (netplan, iwd).
    Unmanaged(String),
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeviceState {
    pub state: u32,
    pub reason: u32,
    /// The active connection's path, `None` for D-Bus's empty `/`.
    pub active_connection: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AccessPoint {
    pub path: String,
    pub ssid: Vec<u8>,
    pub flags: u32,
    pub wpa_flags: u32,
    pub rsn_flags: u32,
    pub strength: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SavedConnection {
    pub path: String,
    pub ssid: Vec<u8>,
}

/// One `StateChanged` signal from the Wi-Fi device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StateChange {
    pub new: u32,
    pub reason: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyMgmt {
    WpaPsk,
    Sae,
    Owe,
}

impl KeyMgmt {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::WpaPsk => "wpa-psk",
            Self::Sae => "sae",
            Self::Owe => "owe",
        }
    }

    pub(super) fn needs_passphrase(self) -> bool {
        !matches!(self, Self::Owe)
    }
}

/// A new connection for NetworkManager to add and activate.
pub(super) struct JoinRequest<'a> {
    pub ssid: &'a str,
    /// `None` for an open network.
    pub key_mgmt: Option<KeyMgmt>,
    pub passphrase: Option<&'a str>,
    pub hidden: bool,
}

impl fmt::Debug for JoinRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinRequest")
            .field("ssid", &self.ssid)
            .field("key_mgmt", &self.key_mgmt)
            .field("passphrase", &self.passphrase.map(|_| "redacted"))
            .field("hidden", &self.hidden)
            .finish()
    }
}

#[async_trait]
pub(super) trait NetworkManager: Send + Sync + 'static {
    async fn wifi_device(&self) -> Result<DeviceLookup, NmError>;
    /// The caller's answers to NetworkManager's polkit actions: `yes`, `no` or `auth`.
    async fn permissions(&self) -> Result<HashMap<String, String>, NmError>;
    /// (`WirelessEnabled`, `WirelessHardwareEnabled`).
    async fn wireless_enabled(&self) -> Result<(bool, bool), NmError>;
    async fn enable_wireless(&self) -> Result<(), NmError>;
    async fn request_scan(&self, device: &str) -> Result<(), NmError>;
    /// Milliseconds since boot of the last finished scan; -1 before the first.
    async fn last_scan(&self, device: &str) -> Result<i64, NmError>;
    async fn access_points(&self, device: &str) -> Result<Vec<AccessPoint>, NmError>;
    async fn saved_connections(&self) -> Result<Vec<SavedConnection>, NmError>;
    async fn device_state(&self, device: &str) -> Result<DeviceState, NmError>;
    async fn active_ssid(&self, device: &str) -> Result<Option<Vec<u8>>, NmError>;
    async fn ipv4(&self, device: &str) -> Result<Option<Ipv4Addr>, NmError>;
    async fn state_changes(&self, device: &str)
        -> Result<BoxStream<'static, StateChange>, NmError>;
    /// Adds the connection in memory only and activates it: (connection, active connection).
    async fn add_and_activate(
        &self,
        request: &JoinRequest<'_>,
        device: &str,
        access_point: Option<&str>,
    ) -> Result<(String, String), NmError>;
    async fn activate(
        &self,
        connection: &str,
        device: &str,
        access_point: Option<&str>,
    ) -> Result<String, NmError>;
    async fn deactivate(&self, active_connection: &str) -> Result<(), NmError>;
    /// Writes an in-memory connection to NetworkManager's own root-only store.
    async fn save(&self, connection: &str) -> Result<(), NmError>;
    async fn delete(&self, connection: &str) -> Result<(), NmError>;
}

pub(super) fn is_activating(state: u32) -> bool {
    (STATE_PREPARE..=STATE_SECONDARIES).contains(&state)
}

/// What a scan shows the phone, from an access point's three flag words.
pub(super) fn security(flags: u32, wpa_flags: u32, rsn_flags: u32) -> WifiSecurity {
    let key_mgmt = wpa_flags | rsn_flags;
    if key_mgmt & (SEC_KEY_MGMT_802_1X | SEC_KEY_MGMT_EAP_SUITE_B_192) != 0 {
        WifiSecurity::Enterprise
    } else if rsn_flags & SEC_KEY_MGMT_SAE != 0 {
        WifiSecurity::Wpa3
    } else if rsn_flags & SEC_KEY_MGMT_PSK != 0 {
        WifiSecurity::Wpa2
    } else if wpa_flags & SEC_KEY_MGMT_PSK != 0 {
        WifiSecurity::Wpa
    } else if key_mgmt & (SEC_KEY_MGMT_OWE | SEC_KEY_MGMT_OWE_TM) != 0 {
        // Enhanced Open: encrypted, but asks for no passphrase, so the phone shows it as open.
        WifiSecurity::Open
    } else if flags & AP_FLAGS_PRIVACY != 0 {
        WifiSecurity::Wep
    } else {
        WifiSecurity::Open
    }
}

/// The key management a join asks for. `None` is an open network; a WPA3 network that also takes
/// WPA2 (transition mode) joins as `wpa-psk`, which every supplicant build supports.
pub(super) fn key_mgmt(ap: &AccessPoint) -> Result<Option<KeyMgmt>, WifiJoinError> {
    let key_mgmt = ap.wpa_flags | ap.rsn_flags;
    match security(ap.flags, ap.wpa_flags, ap.rsn_flags) {
        WifiSecurity::Enterprise | WifiSecurity::Wep => Err(WifiJoinError::Unsupported),
        WifiSecurity::Wpa | WifiSecurity::Wpa2 => Ok(Some(KeyMgmt::WpaPsk)),
        WifiSecurity::Wpa3 if key_mgmt & SEC_KEY_MGMT_PSK != 0 => Ok(Some(KeyMgmt::WpaPsk)),
        WifiSecurity::Wpa3 => Ok(Some(KeyMgmt::Sae)),
        WifiSecurity::Open if key_mgmt & (SEC_KEY_MGMT_OWE | SEC_KEY_MGMT_OWE_TM) != 0 => {
            Ok(Some(KeyMgmt::Owe))
        }
        WifiSecurity::Open => Ok(None),
    }
}

/// The phone's list: one entry per access point NetworkManager sees, `known` when a saved
/// connection names its SSID. A hidden or non-UTF-8 SSID cannot be shown or asked for by name.
pub(super) fn networks(
    access_points: &[AccessPoint],
    known: &HashSet<Vec<u8>>,
) -> Vec<WifiNetwork> {
    access_points
        .iter()
        .filter_map(|ap| {
            let ssid = String::from_utf8(ap.ssid.clone()).ok()?;
            Some(WifiNetwork {
                known: known.contains(&ap.ssid),
                security: security(ap.flags, ap.wpa_flags, ap.rsn_flags),
                signal: ap.strength.min(MAX_SIGNAL),
                ssid,
            })
        })
        .collect()
}

/// A failed activation's `NMDeviceStateReason`, as the spec's code. Logged as a number, because
/// the fallback (`timeout`, "try again") is a guess the Jetson run should be able to correct.
pub(super) fn join_error_from_reason(reason: u32) -> WifiJoinError {
    match reason {
        // no-secrets, supplicant-disconnect, supplicant-config-failed, supplicant-failed
        7..=10 => WifiJoinError::WrongPassword,
        // ssid-not-found
        53 => WifiJoinError::NotFound,
        // ip-config-unavailable, ip-config-expired, dhcp-*, autoip-*, ip-address-duplicate
        5 | 6 | 15..=17 | 20..=22 | 64 => WifiJoinError::NoAddress,
        // sleeping, user-requested, new-activation: something else took the device
        37 | 39 | 60 => WifiJoinError::Busy,
        // now-unmanaged, firmware-missing, removed
        3 | 35 | 36 => WifiJoinError::Unsupported,
        // supplicant-timeout and anything unforeseen
        _ => WifiJoinError::Timeout,
    }
}

/// A refused join call, as the spec's code.
pub(super) fn join_error(error: &NmError) -> WifiJoinError {
    match error {
        NmError::Unreachable(_) => WifiJoinError::Unsupported,
        NmError::Refused(name) if is_permission_denied(name) => WifiJoinError::NotPermitted,
        // The SSID is checked before it gets here and every other field is fixed, so an invalid
        // property is the passphrase (WPA-PSK takes 8 to 63 characters or 64 hex digits).
        NmError::Refused(name) if name.ends_with(".InvalidProperty") => {
            WifiJoinError::WrongPassword
        }
        NmError::Refused(name) if name.ends_with(".UnknownDevice") => WifiJoinError::Unsupported,
        NmError::Refused(name)
            if name.ends_with(".ConnectionAlreadyActive") || name.ends_with(".NotAllowed") =>
        {
            WifiJoinError::Busy
        }
        NmError::Refused(_) => WifiJoinError::Timeout,
    }
}

pub(super) fn scan_error(error: &NmError) -> WifiScanError {
    match error {
        NmError::Refused(name) if is_permission_denied(name) => WifiScanError::NotPermitted,
        NmError::Refused(name) if name.ends_with(".NotAllowed") => WifiScanError::Busy,
        _ => WifiScanError::Unsupported,
    }
}

pub(super) fn is_permission_denied(name: &str) -> bool {
    name.ends_with(".PermissionDenied") || name == "org.freedesktop.DBus.Error.AccessDenied"
}

/// Scanning works without the grant (NetworkManager's own periodic scan), joining does not.
pub(super) fn capabilities(permissions: &HashMap<String, String>) -> WifiCapabilities {
    let granted = |action: &str| permissions.get(action).map(String::as_str) == Some("yes");
    let join = if granted(PERMISSION_MODIFY_SYSTEM) && granted(PERMISSION_NETWORK_CONTROL) {
        Availability::Available
    } else {
        Availability::unavailable(NO_GRANT)
    };
    WifiCapabilities {
        scan: Availability::Available,
        join,
    }
}

pub(super) fn unreachable_reason(error: &NmError) -> String {
    match error {
        NmError::Unreachable(reason) => reason.clone(),
        NmError::Refused(name) => format!("NetworkManager refused: {name}"),
    }
}

#[cfg(test)]
mod tests {
    use pond_core::network::domain::wifi::rank_networks;

    use super::*;

    const PAIR_CCMP: u32 = 0x8;
    const GROUP_CCMP: u32 = 0x80;
    const WPA2_PSK: u32 = SEC_KEY_MGMT_PSK | PAIR_CCMP | GROUP_CCMP;

    fn ap(ssid: &str, flags: u32, wpa: u32, rsn: u32, strength: u8) -> AccessPoint {
        AccessPoint {
            path: format!("/ap/{ssid}/{strength}"),
            ssid: ssid.as_bytes().to_vec(),
            flags,
            wpa_flags: wpa,
            rsn_flags: rsn,
            strength,
        }
    }

    #[test]
    fn flags_read_as_the_security_the_phone_shows() {
        let cases = [
            ((0, 0, 0), WifiSecurity::Open),
            ((AP_FLAGS_PRIVACY, 0, 0), WifiSecurity::Wep),
            (
                (AP_FLAGS_PRIVACY, SEC_KEY_MGMT_PSK | 0x4 | 0x40, 0),
                WifiSecurity::Wpa,
            ),
            ((AP_FLAGS_PRIVACY, 0, WPA2_PSK), WifiSecurity::Wpa2),
            (
                (AP_FLAGS_PRIVACY, SEC_KEY_MGMT_PSK, WPA2_PSK),
                WifiSecurity::Wpa2,
            ),
            (
                (AP_FLAGS_PRIVACY, 0, SEC_KEY_MGMT_SAE | PAIR_CCMP),
                WifiSecurity::Wpa3,
            ),
            (
                (AP_FLAGS_PRIVACY, 0, SEC_KEY_MGMT_SAE | WPA2_PSK),
                WifiSecurity::Wpa3,
            ),
            (
                (AP_FLAGS_PRIVACY, 0, SEC_KEY_MGMT_802_1X | PAIR_CCMP),
                WifiSecurity::Enterprise,
            ),
            (
                (AP_FLAGS_PRIVACY, SEC_KEY_MGMT_802_1X, 0),
                WifiSecurity::Enterprise,
            ),
            (
                (AP_FLAGS_PRIVACY, 0, SEC_KEY_MGMT_EAP_SUITE_B_192),
                WifiSecurity::Enterprise,
            ),
            (
                (AP_FLAGS_PRIVACY, 0, SEC_KEY_MGMT_OWE | PAIR_CCMP),
                WifiSecurity::Open,
            ),
            ((0, 0, SEC_KEY_MGMT_OWE_TM), WifiSecurity::Open),
        ];
        for ((flags, wpa, rsn), expected) in cases {
            assert_eq!(
                security(flags, wpa, rsn),
                expected,
                "flags {flags:#x} wpa {wpa:#x} rsn {rsn:#x}"
            );
        }
    }

    #[test]
    fn a_join_asks_for_the_key_management_the_network_takes() {
        let km = |flags, wpa, rsn| key_mgmt(&ap("Home", flags, wpa, rsn, 50));
        assert_eq!(km(0, 0, 0), Ok(None));
        assert_eq!(km(AP_FLAGS_PRIVACY, 0, WPA2_PSK), Ok(Some(KeyMgmt::WpaPsk)));
        assert_eq!(
            km(AP_FLAGS_PRIVACY, SEC_KEY_MGMT_PSK, 0),
            Ok(Some(KeyMgmt::WpaPsk))
        );
        assert_eq!(
            km(AP_FLAGS_PRIVACY, 0, SEC_KEY_MGMT_SAE | WPA2_PSK),
            Ok(Some(KeyMgmt::WpaPsk)),
            "transition mode joins as WPA2"
        );
        assert_eq!(
            km(AP_FLAGS_PRIVACY, 0, SEC_KEY_MGMT_SAE),
            Ok(Some(KeyMgmt::Sae))
        );
        assert_eq!(km(0, 0, SEC_KEY_MGMT_OWE), Ok(Some(KeyMgmt::Owe)));
        assert_eq!(
            km(AP_FLAGS_PRIVACY, 0, SEC_KEY_MGMT_802_1X),
            Err(WifiJoinError::Unsupported),
            "a passphrase alone cannot join an enterprise network"
        );
        assert_eq!(km(AP_FLAGS_PRIVACY, 0, 0), Err(WifiJoinError::Unsupported));
        assert!(KeyMgmt::Sae.needs_passphrase() && !KeyMgmt::Owe.needs_passphrase());
    }

    #[test]
    fn a_scan_becomes_one_entry_per_ssid_strongest_first_with_known_ones_marked() {
        let aps = [
            ap("Home", AP_FLAGS_PRIVACY, 0, WPA2_PSK, 41),
            ap("Home", AP_FLAGS_PRIVACY, 0, WPA2_PSK, 77),
            ap("Cafe", 0, 0, 0, 63),
            ap("", AP_FLAGS_PRIVACY, 0, WPA2_PSK, 99),
            ap("Attic", AP_FLAGS_PRIVACY, 0, SEC_KEY_MGMT_SAE, 77),
            ap("Loud", 0, 0, 0, 250),
        ];
        let mut invalid = ap("bad", 0, 0, 0, 90);
        invalid.ssid = vec![0xff, 0xfe];
        let mut all = aps.to_vec();
        all.push(invalid);
        let known = HashSet::from([b"Home".to_vec()]);

        let ranked = rank_networks(networks(&all, &known));
        let order: Vec<(&str, WifiSecurity, u8, bool)> = ranked
            .iter()
            .map(|n| (n.ssid.as_str(), n.security, n.signal, n.known))
            .collect();
        assert_eq!(
            order,
            vec![
                ("Loud", WifiSecurity::Open, MAX_SIGNAL, false),
                ("Attic", WifiSecurity::Wpa3, 77, false),
                ("Home", WifiSecurity::Wpa2, 77, true),
                ("Cafe", WifiSecurity::Open, 63, false),
            ],
            "mesh nodes collapse, hidden and non-UTF-8 names drop"
        );
    }

    #[test]
    fn device_state_reasons_read_as_the_specs_codes() {
        let cases = [
            (7, WifiJoinError::WrongPassword),
            (8, WifiJoinError::WrongPassword),
            (9, WifiJoinError::WrongPassword),
            (10, WifiJoinError::WrongPassword),
            (11, WifiJoinError::Timeout),
            (53, WifiJoinError::NotFound),
            (5, WifiJoinError::NoAddress),
            (6, WifiJoinError::NoAddress),
            (15, WifiJoinError::NoAddress),
            (16, WifiJoinError::NoAddress),
            (17, WifiJoinError::NoAddress),
            (64, WifiJoinError::NoAddress),
            (39, WifiJoinError::Busy),
            (60, WifiJoinError::Busy),
            (36, WifiJoinError::Unsupported),
            (1, WifiJoinError::Timeout),
            (4, WifiJoinError::Timeout),
        ];
        for (reason, code) in cases {
            assert_eq!(join_error_from_reason(reason), code, "reason {reason}");
        }
    }

    #[test]
    fn refused_calls_read_as_the_specs_codes() {
        let refused = |name: &str| NmError::Refused(name.to_string());
        let cases = [
            (
                "org.freedesktop.NetworkManager.PermissionDenied",
                WifiJoinError::NotPermitted,
            ),
            (
                "org.freedesktop.NetworkManager.Settings.PermissionDenied",
                WifiJoinError::NotPermitted,
            ),
            (
                "org.freedesktop.DBus.Error.AccessDenied",
                WifiJoinError::NotPermitted,
            ),
            (
                "org.freedesktop.NetworkManager.Settings.Connection.InvalidProperty",
                WifiJoinError::WrongPassword,
            ),
            (
                "org.freedesktop.NetworkManager.UnknownDevice",
                WifiJoinError::Unsupported,
            ),
            (
                "org.freedesktop.NetworkManager.ConnectionAlreadyActive",
                WifiJoinError::Busy,
            ),
            (
                "org.freedesktop.NetworkManager.Device.NotAllowed",
                WifiJoinError::Busy,
            ),
            (
                "org.freedesktop.NetworkManager.Failed",
                WifiJoinError::Timeout,
            ),
        ];
        for (name, code) in cases {
            assert_eq!(join_error(&refused(name)), code, "{name}");
        }
        assert_eq!(
            join_error(&NmError::Unreachable(
                "NetworkManager is not running".into()
            )),
            WifiJoinError::Unsupported
        );
        assert_eq!(
            scan_error(&refused("org.freedesktop.NetworkManager.PermissionDenied")),
            WifiScanError::NotPermitted
        );
        assert_eq!(
            scan_error(&refused("org.freedesktop.NetworkManager.Device.NotAllowed")),
            WifiScanError::Busy
        );
    }

    #[test]
    fn joining_needs_both_grants_and_scanning_needs_neither() {
        let mut permissions = HashMap::new();
        assert_eq!(capabilities(&permissions).join.reason(), Some(NO_GRANT));
        assert!(capabilities(&permissions).scan.is_available());

        permissions.insert(PERMISSION_MODIFY_SYSTEM.to_string(), "yes".to_string());
        permissions.insert(PERMISSION_NETWORK_CONTROL.to_string(), "auth".to_string());
        assert_eq!(
            capabilities(&permissions).join.reason(),
            Some(NO_GRANT),
            "auth needs a polkit agent a headless pond does not have"
        );
        permissions.insert(PERMISSION_NETWORK_CONTROL.to_string(), "yes".to_string());
        assert!(capabilities(&permissions).join.is_available());
    }

    #[test]
    fn a_join_request_never_prints_its_passphrase() {
        let request = JoinRequest {
            ssid: "Home",
            key_mgmt: Some(KeyMgmt::WpaPsk),
            passphrase: Some("otter-lantern-quietly-91"),
            hidden: false,
        };
        let printed = format!("{request:?}");
        assert!(!printed.contains("otter"), "{printed}");
        assert!(printed.contains("redacted") && printed.contains("Home"));
    }
}
