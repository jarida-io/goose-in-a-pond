//! What the pond's Wi-Fi station sees and does, in the setup channel's wire shapes
//! (`docs/architecture/pond-setup-channel.md`, "After the exchange").

use std::collections::HashMap;
use std::fmt;
use std::net::Ipv4Addr;

use serde::{Deserialize, Deserializer, Serialize};

use crate::network::domain::capability::Availability;
use crate::shared::domain::wipe::wipe_string;

/// The strongest signal a network reports, as a percentage.
pub const MAX_SIGNAL: u8 = 100;
/// 802.11 caps an SSID at 32 octets.
pub const MAX_SSID_BYTES: usize = 32;
/// WPA allows 63 characters, or 64 hex digits for a raw key.
pub const MAX_PASSPHRASE_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WifiSecurity {
    Open,
    Wep,
    Wpa,
    Wpa2,
    Wpa3,
    Enterprise,
}

/// One network the station can see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WifiNetwork {
    pub ssid: String,
    pub security: WifiSecurity,
    /// 0 to [`MAX_SIGNAL`].
    pub signal: u8,
    /// The OS already holds credentials for it.
    pub known: bool,
}

/// An SSID the station can be asked to join.
pub fn is_valid_ssid(ssid: &str) -> bool {
    !ssid.is_empty() && ssid.len() <= MAX_SSID_BYTES
}

/// One entry per SSID, strongest first, as the phone shows them. Every access point of a network
/// reports separately, and a hidden one reports no name at all.
pub fn rank_networks(networks: Vec<WifiNetwork>) -> Vec<WifiNetwork> {
    let mut by_ssid: HashMap<String, WifiNetwork> = HashMap::with_capacity(networks.len());
    for mut network in networks {
        if !is_valid_ssid(&network.ssid) {
            continue;
        }
        network.signal = network.signal.min(MAX_SIGNAL);
        match by_ssid.get_mut(&network.ssid) {
            Some(kept) => {
                let known = kept.known || network.known;
                if network.signal > kept.signal {
                    *kept = network;
                }
                kept.known = known;
            }
            None => {
                by_ssid.insert(network.ssid.clone(), network);
            }
        }
    }
    let mut ranked: Vec<WifiNetwork> = by_ssid.into_values().collect();
    ranked.sort_by(|a, b| b.signal.cmp(&a.signal).then_with(|| a.ssid.cmp(&b.ssid)));
    ranked
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a Wi-Fi passphrase is 1 to {MAX_PASSPHRASE_BYTES} bytes")]
pub struct InvalidPassphrase;

/// On its way to the OS's own credential store. No `Display`, `Serialize` or `Clone` and a
/// redacting `Debug`, so it is never logged, returned or copied; wiped on drop.
pub struct WifiPassphrase(String);

impl WifiPassphrase {
    pub fn new(mut text: String) -> Result<Self, InvalidPassphrase> {
        if text.is_empty() || text.len() > MAX_PASSPHRASE_BYTES {
            wipe_string(&mut text);
            return Err(InvalidPassphrase);
        }
        Ok(Self(text))
    }

    /// For the adapter that hands it to the OS, and for nothing else.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for WifiPassphrase {
    fn drop(&mut self) {
        wipe_string(&mut self.0);
    }
}

impl fmt::Debug for WifiPassphrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WifiPassphrase(redacted)")
    }
}

impl<'de> Deserialize<'de> for WifiPassphrase {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // The error names the rule, never the input.
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// The network a join ended on, with the address the phone carries on to over HTTPS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinedNetwork {
    pub ssid: String,
    pub ipv4: Ipv4Addr,
}

/// Why a join failed. Each maps to one wire code the phone branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WifiJoinError {
    #[error("the network refused the passphrase")]
    WrongPassword,
    #[error("no network with that name is in range")]
    NotFound,
    #[error("joined, but no IPv4 address was assigned")]
    NoAddress,
    #[error("the network did not answer in time")]
    Timeout,
    #[error("this pond is not allowed to change its Wi-Fi")]
    NotPermitted,
    #[error("this host cannot join a Wi-Fi network")]
    Unsupported,
    #[error("the Wi-Fi station is busy")]
    Busy,
}

impl WifiJoinError {
    pub fn code(self) -> &'static str {
        match self {
            Self::WrongPassword => "wrong_password",
            Self::NotFound => "not_found",
            Self::NoAddress => "no_address",
            Self::Timeout => "timeout",
            Self::NotPermitted => "not_permitted",
            Self::Unsupported => "unsupported",
            Self::Busy => "busy",
        }
    }
}

/// Why a scan failed, in the join's codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WifiScanError {
    #[error("this pond is not allowed to scan for Wi-Fi")]
    NotPermitted,
    #[error("this host cannot scan for Wi-Fi")]
    Unsupported,
    #[error("the Wi-Fi station is busy")]
    Busy,
}

impl WifiScanError {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotPermitted => "not_permitted",
            Self::Unsupported => "unsupported",
            Self::Busy => "busy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkState {
    Connected,
    Connecting,
    Disconnected,
}

/// The station's link as `status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub state: NetworkState,
    pub ssid: Option<String>,
    pub ipv4: Option<Ipv4Addr>,
}

impl NetworkStatus {
    pub fn disconnected() -> Self {
        Self {
            state: NetworkState::Disconnected,
            ssid: None,
            ipv4: None,
        }
    }
}

/// What a station can do on this host now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiCapabilities {
    pub scan: Availability,
    pub join: Availability,
}

impl WifiCapabilities {
    pub fn available() -> Self {
        Self {
            scan: Availability::Available,
            join: Availability::Available,
        }
    }

    pub fn unsupported(reason: &str) -> Self {
        Self {
            scan: Availability::unavailable(reason),
            join: Availability::unavailable(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(ssid: &str, signal: u8, known: bool) -> WifiNetwork {
        WifiNetwork {
            ssid: ssid.into(),
            security: WifiSecurity::Wpa2,
            signal,
            known,
        }
    }

    #[test]
    fn ranking_keeps_the_strongest_entry_per_ssid_and_orders_strongest_first() {
        let ranked = rank_networks(vec![
            network("Kitchen", 40, false),
            network("Home", 30, true),
            network("Home", 80, false),
            network("Attic", 80, false),
            network("", 99, false),
            network(&"x".repeat(MAX_SSID_BYTES + 1), 99, false),
            network("Loud", 250, false),
        ]);
        let order: Vec<(&str, u8, bool)> = ranked
            .iter()
            .map(|n| (n.ssid.as_str(), n.signal, n.known))
            .collect();
        assert_eq!(
            order,
            vec![
                ("Loud", MAX_SIGNAL, false),
                ("Attic", 80, false),
                ("Home", 80, true),
                ("Kitchen", 40, false),
            ],
            "hidden and oversized names drop, ties order by name, a known entry stays known"
        );
    }

    #[test]
    fn security_and_status_use_the_wire_names() {
        assert_eq!(
            serde_json::to_value([
                WifiSecurity::Open,
                WifiSecurity::Wep,
                WifiSecurity::Wpa,
                WifiSecurity::Wpa2,
                WifiSecurity::Wpa3,
                WifiSecurity::Enterprise
            ])
            .unwrap(),
            serde_json::json!(["open", "wep", "wpa", "wpa2", "wpa3", "enterprise"])
        );
        let status = NetworkStatus {
            state: NetworkState::Connected,
            ssid: Some("Home".into()),
            ipv4: Some(Ipv4Addr::new(192, 168, 1, 42)),
        };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({"state": "connected", "ssid": "Home", "ipv4": "192.168.1.42"})
        );
    }

    #[test]
    fn error_codes_are_the_specs() {
        let join: Vec<&str> = [
            WifiJoinError::WrongPassword,
            WifiJoinError::NotFound,
            WifiJoinError::NoAddress,
            WifiJoinError::Timeout,
            WifiJoinError::NotPermitted,
            WifiJoinError::Unsupported,
            WifiJoinError::Busy,
        ]
        .into_iter()
        .map(WifiJoinError::code)
        .collect();
        assert_eq!(
            join,
            [
                "wrong_password",
                "not_found",
                "no_address",
                "timeout",
                "not_permitted",
                "unsupported",
                "busy"
            ]
        );
        assert_eq!(WifiScanError::NotPermitted.code(), "not_permitted");
        assert_eq!(WifiScanError::Unsupported.code(), "unsupported");
        assert_eq!(WifiScanError::Busy.code(), "busy");
    }

    #[test]
    fn a_passphrase_never_prints_and_is_bounded() {
        let passphrase = WifiPassphrase::new("correct horse battery".into()).unwrap();
        assert_eq!(passphrase.expose(), "correct horse battery");
        assert_eq!(format!("{passphrase:?}"), "WifiPassphrase(redacted)");
        assert!(WifiPassphrase::new(String::new()).is_err());
        assert!(WifiPassphrase::new("x".repeat(MAX_PASSPHRASE_BYTES + 1)).is_err());
        assert!(WifiPassphrase::new("x".repeat(MAX_PASSPHRASE_BYTES)).is_ok());
    }

    #[test]
    fn a_passphrase_deserialises_and_a_bad_one_does_not_echo_itself() {
        #[derive(Debug, Deserialize)]
        struct Join {
            passphrase: Option<WifiPassphrase>,
        }
        let join: Join = serde_json::from_str(r#"{"passphrase": "hunter2hunter2"}"#).unwrap();
        assert_eq!(join.passphrase.as_ref().unwrap().expose(), "hunter2hunter2");
        assert!(!format!("{join:?}").contains("hunter2"));

        let long = "y".repeat(MAX_PASSPHRASE_BYTES + 1);
        let error = serde_json::from_str::<Join>(&format!(r#"{{"passphrase": "{long}"}}"#))
            .unwrap_err()
            .to_string();
        assert!(!error.contains(&long), "{error}");
    }

    #[test]
    fn ssids_are_one_to_thirty_two_bytes() {
        assert!(!is_valid_ssid(""));
        assert!(is_valid_ssid("Home"));
        assert!(is_valid_ssid(&"x".repeat(MAX_SSID_BYTES)));
        assert!(!is_valid_ssid(&"x".repeat(MAX_SSID_BYTES + 1)));
    }
}
