//! What this host can do for a phone during setup, probed at runtime, with the reason when it
//! cannot, so the phone and the dashboard offer only what works.

use pond_core::network::domain::capability::Availability;
use pond_core::network::ports::wifi_station::WifiStation;
use pond_core::network::services::spoken_code::SpokenCodeService;

pub const WIFI_SCAN: &str = "wifi_scan";
pub const WIFI_JOIN: &str = "wifi_join";
pub const SPOKEN_CODE: &str = "spoken_code";

const NO_STATION: &str = "this host has no Wi-Fi station the pond can drive";
const NO_SPEAKER: &str = "this pond cannot make a sound";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupCapabilities {
    pub wifi_scan: Availability,
    pub wifi_join: Availability,
    pub spoken_code: Availability,
}

impl SetupCapabilities {
    pub async fn probe(
        station: Option<&dyn WifiStation>,
        spoken_codes: &SpokenCodeService,
    ) -> Self {
        let (wifi_scan, wifi_join) = match station {
            Some(station) => {
                let wifi = station.capabilities().await;
                (wifi.scan, wifi.join)
            }
            None => (
                Availability::unavailable(NO_STATION),
                Availability::unavailable(NO_STATION),
            ),
        };
        let spoken_code = if spoken_codes.can_announce().await {
            Availability::Available
        } else {
            Availability::unavailable(NO_SPEAKER)
        };
        Self {
            wifi_scan,
            wifi_join,
            spoken_code,
        }
    }

    /// The wire names `hello` lists, in a fixed order.
    pub fn offered(&self) -> Vec<&'static str> {
        [
            (WIFI_SCAN, &self.wifi_scan),
            (WIFI_JOIN, &self.wifi_join),
            (SPOKEN_CODE, &self.spoken_code),
        ]
        .into_iter()
        .filter(|(_, availability)| availability.is_available())
        .map(|(name, _)| name)
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pond_core::network::domain::wifi::WifiCapabilities;
    use pond_core::network::mocks::mock_code_announcer::MockCodeAnnouncer;
    use pond_core::network::mocks::mock_wifi_station::MockWifiStation;

    use super::*;

    #[tokio::test]
    async fn a_host_offers_what_its_station_and_speaker_can_do() {
        let speaker = SpokenCodeService::new(Some(Arc::new(MockCodeAnnouncer::new())));
        let station = MockWifiStation::new();
        let all = SetupCapabilities::probe(Some(&station), &speaker).await;
        assert_eq!(all.offered(), [WIFI_SCAN, WIFI_JOIN, SPOKEN_CODE]);

        station.set_capabilities(WifiCapabilities {
            scan: Availability::unavailable("Location permission is off"),
            join: Availability::Available,
        });
        let partial = SetupCapabilities::probe(Some(&station), &SpokenCodeService::default()).await;
        assert_eq!(partial.offered(), [WIFI_JOIN]);
        assert_eq!(
            partial.wifi_scan.reason(),
            Some("Location permission is off")
        );
        assert_eq!(partial.spoken_code.reason(), Some(NO_SPEAKER));

        let none = SetupCapabilities::probe(None, &SpokenCodeService::default()).await;
        assert!(none.offered().is_empty());
        assert_eq!(none.wifi_join.reason(), Some(NO_STATION));
    }
}
