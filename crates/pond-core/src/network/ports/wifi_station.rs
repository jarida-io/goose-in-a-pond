use async_trait::async_trait;

use crate::network::domain::wifi::{
    CurrentNetwork, JoinedNetwork, NetworkStatus, WifiCapabilities, WifiForgetError, WifiJoinError,
    WifiNetwork, WifiPassphrase, WifiScanError,
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

    /// The saved network the station is on now; `None` when it is on none, or cannot tell.
    async fn current(&self) -> Option<CurrentNetwork>;

    /// Deletes every saved network named `ssid`, which drops the link if the pond is on it. The
    /// caller decides whether that is allowed; the station only does it.
    async fn forget(&self, ssid: &str) -> Result<(), WifiForgetError>;

    /// Deletes every saved network (factory reset). Default refuses as `Unsupported`: no adapter
    /// enumerates its saved networks today, so this needs a per-OS implementation before it can
    /// do more than refuse.
    async fn forget_all(&self) -> Result<(), WifiForgetError> {
        Err(WifiForgetError::Unsupported)
    }
}
