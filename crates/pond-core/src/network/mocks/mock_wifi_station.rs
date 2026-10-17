//! In-memory [`WifiStation`] standing in for the OS network manager.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::network::domain::wifi::{
    JoinedNetwork, NetworkState, NetworkStatus, WifiCapabilities, WifiJoinError, WifiNetwork,
    WifiPassphrase, WifiScanError, WifiSecurity,
};
use crate::network::ports::wifi_station::WifiStation;

/// One `join` as the station received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinAttempt {
    pub ssid: String,
    /// Kept so a test can prove the passphrase arrived intact; a real station never keeps it.
    pub passphrase: Option<String>,
    pub hidden: bool,
}

struct Reachable {
    passphrase: Option<String>,
    hidden: bool,
}

pub struct MockWifiStation {
    visible: Mutex<Vec<WifiNetwork>>,
    reachable: Mutex<HashMap<String, Reachable>>,
    status: Mutex<NetworkStatus>,
    joins: Mutex<Vec<JoinAttempt>>,
    capabilities: Mutex<WifiCapabilities>,
    scan_error: Mutex<Option<WifiScanError>>,
    join_error: Mutex<Option<WifiJoinError>>,
}

impl MockWifiStation {
    /// The address every successful join is given.
    pub const ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 42);

    /// A station that can scan and join, sees nothing and is disconnected.
    pub fn new() -> Self {
        Self {
            visible: Mutex::new(Vec::new()),
            reachable: Mutex::new(HashMap::new()),
            status: Mutex::new(NetworkStatus::disconnected()),
            joins: Mutex::new(Vec::new()),
            capabilities: Mutex::new(WifiCapabilities::available()),
            scan_error: Mutex::new(None),
            join_error: Mutex::new(None),
        }
    }

    /// A network in range that a scan reports; `passphrase` is `None` for an open one.
    pub fn with_network(self, network: WifiNetwork, passphrase: Option<&str>) -> Self {
        self.reachable.lock().unwrap().insert(
            network.ssid.clone(),
            Reachable {
                passphrase: passphrase.map(str::to_string),
                hidden: false,
            },
        );
        self.visible.lock().unwrap().push(network);
        self
    }

    /// A network in range that no scan reports, joinable only when asked for as hidden.
    pub fn with_hidden_network(self, ssid: &str, passphrase: Option<&str>) -> Self {
        self.reachable.lock().unwrap().insert(
            ssid.to_string(),
            Reachable {
                passphrase: passphrase.map(str::to_string),
                hidden: true,
            },
        );
        self
    }

    pub fn set_capabilities(&self, capabilities: WifiCapabilities) {
        *self.capabilities.lock().unwrap() = capabilities;
    }

    pub fn fail_scans(&self, error: Option<WifiScanError>) {
        *self.scan_error.lock().unwrap() = error;
    }

    pub fn fail_joins(&self, error: Option<WifiJoinError>) {
        *self.join_error.lock().unwrap() = error;
    }

    pub fn joins(&self) -> Vec<JoinAttempt> {
        self.joins.lock().unwrap().clone()
    }
}

impl Default for MockWifiStation {
    fn default() -> Self {
        Self::new()
    }
}

/// The mock's own idea of a plausible scan, for tests that only need some networks.
pub fn sample_network(ssid: &str, security: WifiSecurity, signal: u8) -> WifiNetwork {
    WifiNetwork {
        ssid: ssid.to_string(),
        security,
        signal,
        known: false,
    }
}

#[async_trait]
impl WifiStation for MockWifiStation {
    async fn capabilities(&self) -> WifiCapabilities {
        self.capabilities.lock().unwrap().clone()
    }

    async fn scan(&self) -> Result<Vec<WifiNetwork>, WifiScanError> {
        if let Some(error) = *self.scan_error.lock().unwrap() {
            return Err(error);
        }
        Ok(self.visible.lock().unwrap().clone())
    }

    async fn join(
        &self,
        ssid: &str,
        passphrase: Option<&WifiPassphrase>,
        hidden: bool,
    ) -> Result<JoinedNetwork, WifiJoinError> {
        self.joins.lock().unwrap().push(JoinAttempt {
            ssid: ssid.to_string(),
            passphrase: passphrase.map(|p| p.expose().to_string()),
            hidden,
        });
        if let Some(error) = *self.join_error.lock().unwrap() {
            return Err(error);
        }
        let outcome = match self.reachable.lock().unwrap().get(ssid) {
            None => Err(WifiJoinError::NotFound),
            Some(network) if network.hidden && !hidden => Err(WifiJoinError::NotFound),
            Some(network) if network.passphrase.as_deref() != passphrase.map(|p| p.expose()) => {
                Err(WifiJoinError::WrongPassword)
            }
            Some(_) => Ok(JoinedNetwork {
                ssid: ssid.to_string(),
                ipv4: Self::ADDRESS,
            }),
        };
        if let Ok(joined) = &outcome {
            *self.status.lock().unwrap() = NetworkStatus {
                state: NetworkState::Connected,
                ssid: Some(joined.ssid.clone()),
                ipv4: Some(joined.ipv4),
            };
        }
        outcome
    }

    async fn status(&self) -> NetworkStatus {
        self.status.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passphrase(text: &str) -> WifiPassphrase {
        WifiPassphrase::new(text.into()).unwrap()
    }

    #[tokio::test]
    async fn it_joins_with_the_right_passphrase_and_reports_the_link() {
        let station = MockWifiStation::new()
            .with_network(
                sample_network("Home", WifiSecurity::Wpa2, 70),
                Some("hunter2hunter2"),
            )
            .with_network(sample_network("Cafe", WifiSecurity::Open, 40), None);
        assert_eq!(station.scan().await.unwrap().len(), 2);
        assert_eq!(station.status().await, NetworkStatus::disconnected());

        assert_eq!(
            station
                .join("Home", Some(&passphrase("wrong-guess")), false)
                .await,
            Err(WifiJoinError::WrongPassword)
        );
        assert_eq!(
            station.join("Nowhere", None, false).await,
            Err(WifiJoinError::NotFound)
        );
        let joined = station
            .join("Home", Some(&passphrase("hunter2hunter2")), false)
            .await
            .unwrap();
        assert_eq!(joined.ipv4, MockWifiStation::ADDRESS);
        assert_eq!(station.status().await.state, NetworkState::Connected);
        assert!(station.join("Cafe", None, false).await.is_ok());
        assert_eq!(
            station.joins()[2].passphrase.as_deref(),
            Some("hunter2hunter2")
        );
    }

    #[tokio::test]
    async fn a_hidden_network_is_found_only_when_asked_for_as_hidden() {
        let station = MockWifiStation::new().with_hidden_network("Attic", None);
        assert!(station.scan().await.unwrap().is_empty());
        assert_eq!(
            station.join("Attic", None, false).await,
            Err(WifiJoinError::NotFound)
        );
        assert!(station.join("Attic", None, true).await.is_ok());
    }

    #[tokio::test]
    async fn forced_failures_and_capabilities_are_reported() {
        let station = MockWifiStation::new();
        station.fail_scans(Some(WifiScanError::NotPermitted));
        station.fail_joins(Some(WifiJoinError::Busy));
        station.set_capabilities(WifiCapabilities::unsupported("no adapter"));
        assert_eq!(station.scan().await, Err(WifiScanError::NotPermitted));
        assert_eq!(
            station.join("Home", None, false).await,
            Err(WifiJoinError::Busy)
        );
        assert!(!station.capabilities().await.scan.is_available());
    }
}
