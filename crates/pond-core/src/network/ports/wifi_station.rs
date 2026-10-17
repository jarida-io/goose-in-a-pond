use async_trait::async_trait;

use crate::network::domain::wifi::{
    JoinedNetwork, NetworkStatus, WifiCapabilities, WifiJoinError, WifiNetwork, WifiPassphrase,
    WifiScanError,
};

/// The pond's Wi-Fi client, driven by the OS network manager. The passphrase goes straight to the
/// OS's own credential store; an adapter never writes it down, logs it or returns it.
#[async_trait]
pub trait WifiStation: Send + Sync {
    /// Probed at runtime: the same build can scan on one host and not another (macOS without
    /// Location permission, Linux without the polkit grant).
    async fn capabilities(&self) -> WifiCapabilities;

    /// Every network in range as the OS reports it; callers rank with
    /// [`crate::network::domain::wifi::rank_networks`].
    async fn scan(&self) -> Result<Vec<WifiNetwork>, WifiScanError>;

    /// `passphrase` is `None` for an open network. `hidden` asks the station to probe for an SSID
    /// that does not broadcast.
    async fn join(
        &self,
        ssid: &str,
        passphrase: Option<&WifiPassphrase>,
        hidden: bool,
    ) -> Result<JoinedNetwork, WifiJoinError>;

    /// A failed read reports `disconnected`, which only ever offers more setup, never less.
    async fn status(&self) -> NetworkStatus;
}
