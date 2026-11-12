//! The pond's Wi-Fi as the household's connections settings drive it over HTTPS
//! (`pond-connections.md`). The station does the work; this decides what a screen is told and
//! when forgetting a network needs the household to confirm it.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::network::domain::wifi::{
    rank_networks, JoinedNetwork, ListedNetwork, NetworkState, WifiCapabilities, WifiForgetError,
    WifiJoinError, WifiLink, WifiPassphrase, WifiScanError,
};
use crate::network::ports::wifi_station::WifiStation;

/// One answer stays small enough for a phone to render at once.
pub const MAX_LISTED_NETWORKS: usize = 50;
/// The contract's bound on a scan; Linux alone waits up to 10 s for a fresh one.
pub const SCAN_BUDGET: Duration = Duration::from_secs(15);
/// The contract's bound on a join, the scan for its network included.
pub const JOIN_BUDGET: Duration = Duration::from_secs(45);

pub struct WifiConnections {
    station: Arc<dyn WifiStation>,
    scan_budget: Duration,
    join_budget: Duration,
}

/// Why a detached station call gave no answer.
enum Unanswered {
    TimedOut,
    Panicked,
}

impl WifiConnections {
    /// `station` must be the instance the setup channel holds, so their operations exclude each
    /// other.
    pub fn new(station: Arc<dyn WifiStation>) -> Self {
        Self {
            station,
            scan_budget: SCAN_BUDGET,
            join_budget: JOIN_BUDGET,
        }
    }

    pub fn with_budgets(mut self, scan: Duration, join: Duration) -> Self {
        self.scan_budget = scan;
        self.join_budget = join;
        self
    }

    /// The link and what the station can do, read together for one screen.
    pub async fn link(&self) -> (WifiLink, WifiCapabilities) {
        let (status, current, capabilities) = tokio::join!(
            self.station.status(),
            self.station.current(),
            self.station.capabilities()
        );
        (
            WifiLink::read(&status, current.as_ref(), &capabilities),
            capabilities,
        )
    }

    /// Scans now: one entry per SSID, strongest first, at most [`MAX_LISTED_NETWORKS`].
    pub async fn networks(&self) -> Result<Vec<ListedNetwork>, WifiScanError> {
        let station = self.station.clone();
        let scanned = match detached(self.scan_budget, async move { station.scan().await }).await {
            Ok(scanned) => scanned?,
            Err(Unanswered::TimedOut) => return Err(WifiScanError::Timeout),
            Err(Unanswered::Panicked) => return Err(WifiScanError::Unsupported),
        };
        let current = self.station.current().await.map(|current| current.ssid);
        let mut ranked = rank_networks(scanned);
        ranked.truncate(MAX_LISTED_NETWORKS);
        Ok(ranked
            .into_iter()
            .map(|network| ListedNetwork::new(network, current.as_deref()))
            .collect())
    }

    /// Runs apart from the caller: a join that moves the pond to another LAN can drop the very
    /// connection that asked, and must still finish, saving or undoing its connection.
    pub async fn join(
        &self,
        ssid: String,
        passphrase: Option<WifiPassphrase>,
        hidden: bool,
    ) -> Result<JoinedNetwork, WifiJoinError> {
        let station = self.station.clone();
        let joining = async move { station.join(&ssid, passphrase.as_ref(), hidden).await };
        match detached(self.join_budget, joining).await {
            Ok(joined) => joined,
            Err(Unanswered::TimedOut) => Err(WifiJoinError::Timeout),
            Err(Unanswered::Panicked) => Err(WifiJoinError::Unsupported),
        }
    }

    /// Forgetting the network the pond is on takes it off the LAN unless another link remains, so
    /// it needs `confirm`. Either reading of "on it" counts, so a station that cannot name its
    /// saved network still protects the live one.
    pub async fn forget(&self, ssid: &str, confirm: bool) -> Result<(), WifiForgetError> {
        if !confirm {
            let (status, current) = tokio::join!(self.station.status(), self.station.current());
            let on_it = current.is_some_and(|current| current.ssid == ssid)
                || (status.state == NetworkState::Connected
                    && status.ssid.as_deref() == Some(ssid));
            if on_it {
                return Err(WifiForgetError::InUse);
            }
        }
        let station = self.station.clone();
        let ssid = ssid.to_owned();
        match tokio::spawn(async move { station.forget(&ssid).await }).await {
            Ok(forgotten) => forgotten,
            Err(error) => {
                tracing::error!(%error, "the Wi-Fi station panicked forgetting a network");
                Err(WifiForgetError::Unsupported)
            }
        }
    }

    /// Deletes every saved network (factory reset); unlike [`Self::forget`] this always drops the
    /// live link too, since a reset has nothing left worth staying connected for.
    pub async fn forget_all(&self) -> Result<(), WifiForgetError> {
        let station = self.station.clone();
        match tokio::spawn(async move { station.forget_all().await }).await {
            Ok(forgotten) => forgotten,
            Err(error) => {
                tracing::error!(%error, "the Wi-Fi station panicked forgetting every network");
                Err(WifiForgetError::Unsupported)
            }
        }
    }
}

/// Runs `work` on its own task and waits up to `budget`; past it, the work carries on and still
/// holds the station, so the next operation is `busy` until it ends.
async fn detached<T: Send + 'static>(
    budget: Duration,
    work: impl Future<Output = T> + Send + 'static,
) -> Result<T, Unanswered> {
    match tokio::time::timeout(budget, tokio::spawn(work)).await {
        Ok(Ok(answer)) => Ok(answer),
        Ok(Err(error)) => {
            tracing::error!(%error, "the Wi-Fi station panicked");
            Err(Unanswered::Panicked)
        }
        Err(_) => {
            tracing::warn!(
                budget_s = budget.as_secs(),
                "the Wi-Fi station did not answer in time"
            );
            Err(Unanswered::TimedOut)
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::network::domain::wifi::{
        CurrentNetwork, NetworkStatus, WifiLinkState, WifiNetwork, WifiSecurity,
    };
    use crate::network::mocks::mock_wifi_station::{sample_network, MockWifiStation};

    /// The mock, each radio operation taking `delay` first, as a real scan or join does.
    struct Slow {
        inner: MockWifiStation,
        delay: Duration,
    }

    #[async_trait]
    impl WifiStation for Slow {
        async fn capabilities(&self) -> WifiCapabilities {
            self.inner.capabilities().await
        }
        async fn scan(&self) -> Result<Vec<WifiNetwork>, WifiScanError> {
            tokio::time::sleep(self.delay).await;
            self.inner.scan().await
        }
        async fn join(
            &self,
            ssid: &str,
            passphrase: Option<&WifiPassphrase>,
            hidden: bool,
        ) -> Result<JoinedNetwork, WifiJoinError> {
            tokio::time::sleep(self.delay).await;
            self.inner.join(ssid, passphrase, hidden).await
        }
        async fn status(&self) -> NetworkStatus {
            self.inner.status().await
        }
        async fn current(&self) -> Option<CurrentNetwork> {
            self.inner.current().await
        }
        async fn forget(&self, ssid: &str) -> Result<(), WifiForgetError> {
            self.inner.forget(ssid).await
        }
    }

    fn passphrase(text: &str) -> WifiPassphrase {
        WifiPassphrase::new(text.into()).unwrap()
    }

    #[tokio::test]
    async fn the_list_is_ranked_capped_and_marks_the_network_the_pond_is_on() {
        let mut station = MockWifiStation::new()
            .with_network(sample_network("Home", WifiSecurity::Wpa2, 40), None)
            .with_network(sample_network("Home", WifiSecurity::Wpa2, 70), None)
            .with_network(sample_network("Cafe", WifiSecurity::Open, 55), None)
            .with_network(sample_network("", WifiSecurity::Open, 99), None)
            .with_saved("Cafe");
        for n in 0..60 {
            station = station.with_network(
                sample_network(&format!("Flat {n:02}"), WifiSecurity::Wpa3, 10),
                None,
            );
        }
        station.connect_to("Home");
        let wifi = WifiConnections::new(Arc::new(station));
        let networks = wifi.networks().await.unwrap();
        assert_eq!(networks.len(), MAX_LISTED_NETWORKS);
        assert_eq!(
            networks[..2],
            [
                ListedNetwork {
                    ssid: "Home".into(),
                    security: WifiSecurity::Wpa2,
                    signal: 70,
                    known: true,
                    current: true,
                },
                ListedNetwork {
                    ssid: "Cafe".into(),
                    security: WifiSecurity::Open,
                    signal: 55,
                    known: true,
                    current: false,
                },
            ]
        );
        assert!(networks.iter().all(|n| !n.ssid.is_empty()));
        assert_eq!(networks.iter().filter(|n| n.current).count(), 1);
    }

    #[tokio::test]
    async fn the_link_reads_status_current_and_capabilities_together() {
        let station = Arc::new(
            MockWifiStation::new()
                .with_network(sample_network("Home", WifiSecurity::Wpa2, 70), None),
        );
        let wifi = WifiConnections::new(station.clone());
        assert_eq!(wifi.link().await.0.state, WifiLinkState::Disconnected);
        station.connect_to("Home");
        let (link, capabilities) = wifi.link().await;
        assert_eq!(link.state, WifiLinkState::Connected);
        assert_eq!(link.ssid.as_deref(), Some("Home"));
        assert_eq!(link.signal, Some(70));
        assert_eq!(link.address, Some(MockWifiStation::ADDRESS));
        assert_eq!(capabilities, WifiCapabilities::available());
    }

    #[tokio::test(start_paused = true)]
    async fn a_scan_past_its_budget_is_a_timeout() {
        let station = Slow {
            inner: MockWifiStation::new(),
            delay: SCAN_BUDGET + Duration::from_secs(1),
        };
        let wifi = WifiConnections::new(Arc::new(station));
        let started = tokio::time::Instant::now();
        assert_eq!(wifi.networks().await, Err(WifiScanError::Timeout));
        assert!(started.elapsed() <= SCAN_BUDGET + Duration::from_millis(1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_join_past_its_budget_answers_timeout_and_still_finishes() {
        let inner = MockWifiStation::new().with_network(
            sample_network("Home", WifiSecurity::Wpa2, 70),
            Some("hunter2hunter2"),
        );
        let station = Arc::new(Slow {
            inner,
            delay: JOIN_BUDGET + Duration::from_secs(5),
        });
        let wifi = WifiConnections::new(station.clone());
        assert_eq!(
            wifi.join("Home".into(), Some(passphrase("hunter2hunter2")), false)
                .await,
            Err(WifiJoinError::Timeout)
        );
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(
            station.inner.current().await.map(|c| c.ssid).as_deref(),
            Some("Home"),
            "the join outlives the request that asked for it"
        );
    }

    #[tokio::test]
    async fn a_join_answers_the_stations_result_and_its_codes() {
        let station = Arc::new(
            MockWifiStation::new()
                .with_network(
                    sample_network("Home", WifiSecurity::Wpa2, 70),
                    Some("hunter2hunter2"),
                )
                .with_network(
                    sample_network("Office", WifiSecurity::Enterprise, 80),
                    Some("hunter2hunter2"),
                ),
        );
        let wifi = WifiConnections::new(station.clone());
        assert_eq!(
            wifi.join("Office".into(), Some(passphrase("hunter2hunter2")), false)
                .await,
            Err(WifiJoinError::UnsupportedSecurity)
        );
        assert_eq!(
            wifi.join("Home".into(), Some(passphrase("wrong-guess")), false)
                .await,
            Err(WifiJoinError::WrongPassword)
        );
        let joined = wifi
            .join("Home".into(), Some(passphrase("hunter2hunter2")), false)
            .await
            .unwrap();
        assert_eq!(joined.ipv4, MockWifiStation::ADDRESS);
        assert_eq!(
            station.joins().last().unwrap().passphrase.as_deref(),
            Some("hunter2hunter2")
        );
    }

    #[tokio::test]
    async fn forgetting_the_current_network_needs_confirm() {
        let station = Arc::new(MockWifiStation::new().with_saved("Cafe"));
        station.connect_to("Home");
        let wifi = WifiConnections::new(station.clone());
        assert_eq!(
            wifi.forget("Home", false).await,
            Err(WifiForgetError::InUse)
        );
        assert!(station.forgotten().is_empty(), "nothing was deleted");
        wifi.forget("Cafe", false).await.unwrap();
        assert_eq!(
            wifi.forget("Attic", false).await,
            Err(WifiForgetError::NotFound)
        );
        wifi.forget("Home", true).await.unwrap();
        assert_eq!(station.forgotten(), ["Cafe", "Home"]);
    }

    /// A station that names the live network but not the saved one it came from.
    struct Nameless(MockWifiStation);

    #[async_trait]
    impl WifiStation for Nameless {
        async fn capabilities(&self) -> WifiCapabilities {
            self.0.capabilities().await
        }
        async fn scan(&self) -> Result<Vec<WifiNetwork>, WifiScanError> {
            self.0.scan().await
        }
        async fn join(
            &self,
            ssid: &str,
            passphrase: Option<&WifiPassphrase>,
            hidden: bool,
        ) -> Result<JoinedNetwork, WifiJoinError> {
            self.0.join(ssid, passphrase, hidden).await
        }
        async fn status(&self) -> NetworkStatus {
            self.0.status().await
        }
        async fn current(&self) -> Option<CurrentNetwork> {
            None
        }
        async fn forget(&self, ssid: &str) -> Result<(), WifiForgetError> {
            self.0.forget(ssid).await
        }
    }

    #[tokio::test]
    async fn the_live_link_alone_is_enough_to_need_confirm() {
        let inner = MockWifiStation::new();
        inner.connect_to("Home");
        let wifi = WifiConnections::new(Arc::new(Nameless(inner)));
        assert_eq!(
            wifi.forget("Home", false).await,
            Err(WifiForgetError::InUse)
        );
    }
}
