//! The BLE setup channel (`docs/architecture/pond-setup-channel.md`), wired from the same handshake,
//! spoken-code limiter, settings, profiles, claims, onboarding and event log the HTTPS API holds,
//! so a phone over Bluetooth meets the same pond. Nothing here can stop the pond: the peripheral
//! starts in the advertising controller's task, and its failure only switches BLE setup off.

use std::sync::Arc;

use pond_api::AppState;
use pond_connect::advertising::AdvertisingController;
use pond_connect::ble::BlePeripheral;
use pond_connect::interfaces::HostNetwork;
use pond_connect::setup_channel::{SetupChannel, SetupChannelConfig, SetupChannelContext};
use pond_connect::status::SetupRadioStatus;
use pond_core::network::ports::setup_radios::SetupRadios;
use pond_core::network::ports::wifi_station::WifiStation;
use pond_core::network::services::setup_claim::SetupClaimService;

/// Builds the channel over `state`'s own ports, spawns the controller that starts and advertises
/// `peripheral`, and returns what the status route reads.
pub fn start(
    state: &AppState,
    config: SetupChannelConfig,
    claims: Arc<SetupClaimService>,
    peripheral: Arc<dyn BlePeripheral>,
    wifi: Arc<dyn WifiStation>,
) -> Arc<dyn SetupRadios> {
    let mut context = SetupChannelContext::new(
        config,
        state.handshake.clone(),
        state.spoken_codes.clone(),
        state.settings_repo.clone(),
        state.profile_repo.clone(),
    )
    .with_claims(claims.clone())
    .with_onboarding(state.onboarding_repo.clone())
    .with_wifi(wifi.clone());
    if let Some(event_log) = state.event_log.clone() {
        context = context.with_event_log(event_log);
    }
    let channel = SetupChannel::new(context);
    let radios = Arc::new(SetupRadioStatus::new(
        peripheral.clone(),
        wifi,
        channel.clone(),
    ));
    let controller = AdvertisingController::new(
        peripheral,
        channel,
        claims,
        state.settings_repo.clone(),
        Arc::new(HostNetwork),
    );
    tokio::spawn(controller.run());
    radios
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use pond_connect::ble::{Advertisement, BleError, PeripheralStatus};
    use pond_core::network::domain::discriminator::Discriminator;
    use pond_core::network::mocks::mock_setup_code_store::MockSetupCodeStore;
    use pond_core::network::mocks::mock_wifi_station::MockWifiStation;

    use super::*;

    /// Refuses to start, as a host without Bluetooth does.
    #[derive(Default)]
    struct NoRadio {
        starts: AtomicUsize,
    }

    #[async_trait]
    impl BlePeripheral for NoRadio {
        async fn start(&self, _: Arc<SetupChannel>) -> Result<(), BleError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Err(BleError::Unavailable("no radio here".into()))
        }
        async fn advertise(&self, _: Option<Advertisement>) -> Result<(), BleError> {
            Err(BleError::Unavailable("no radio here".into()))
        }
        async fn shutdown(&self) {}
        fn status(&self) -> PeripheralStatus {
            PeripheralStatus::unavailable("no radio here")
        }
    }

    #[tokio::test]
    async fn a_host_without_bluetooth_keeps_serving_and_says_why() {
        let pond_api::test_support::TestState {
            state, dir: _dir, ..
        } = pond_api::test_support::app_state().await;
        let claims = Arc::new(SetupClaimService::new(Arc::new(
            MockSetupCodeStore::unclaimed(),
        )));
        let peripheral = Arc::new(NoRadio::default());
        let radios = start(
            &state,
            SetupChannelConfig {
                discriminator: Discriminator::new(0xc1cd),
                pin: pond_api::test_support::TEST_SPKI_PIN.into(),
                https_port: 4443,
                hostname: "pond".into(),
            },
            claims,
            peripheral.clone(),
            Arc::new(MockWifiStation::new()),
        );
        for _ in 0..50 {
            if peripheral.starts.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(peripheral.starts.load(Ordering::SeqCst), 1);
        assert_eq!(radios.ble().reason(), Some("no radio here"));
        assert!(!radios.session_open());
        assert!(radios.wifi().await.join.is_available());
    }
}
