//! When the pond advertises the setup channel (`pond-setup-channel.md`, "Advertising"). It
//! re-reads the claim, the network and the household's switch every
//! [`ADVERTISING_CHECK_INTERVAL`] and starts or stops advertising. It never reaches a session: the
//! handle belongs to the connection, so stopping here leaves an open session running. It also
//! starts the peripheral, the first time the switch reads on, so a household that turned BLE setup
//! off never has the radio touched at all.

use std::sync::Arc;

use pond_core::network::services::advertising::{should_advertise, ADVERTISING_CHECK_INTERVAL};
use pond_core::network::services::setup_claim::SetupClaimService;
use pond_core::user_data::ports::settings::SettingsRepository;

use crate::ble::{local_name, Advertisement, BlePeripheral};
use crate::interfaces::NetworkProbe;
use crate::setup_channel::SetupChannel;

/// What one check saw and decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub enabled: bool,
    pub claimed: bool,
    pub has_usable_network: bool,
    pub advertise: bool,
}

pub struct AdvertisingController {
    peripheral: Arc<dyn BlePeripheral>,
    channel: SetupChannel,
    claims: Arc<SetupClaimService>,
    settings: Arc<dyn SettingsRepository>,
    network: Arc<dyn NetworkProbe>,
    advertisement: Advertisement,
    /// Before the first successful read the radio stays quiet: a household that switched it off
    /// must never see it advertise because the database was busy at startup.
    enabled: bool,
    /// What the peripheral last accepted; `None` retries on the next check.
    applied: Option<bool>,
    last_error: Option<String>,
    started: Option<Result<(), ()>>,
}

impl AdvertisingController {
    pub fn new(
        peripheral: Arc<dyn BlePeripheral>,
        channel: SetupChannel,
        claims: Arc<SetupClaimService>,
        settings: Arc<dyn SettingsRepository>,
        network: Arc<dyn NetworkProbe>,
    ) -> Self {
        let advertisement = Advertisement {
            local_name: local_name(channel.config().discriminator),
        };
        Self {
            peripheral,
            channel,
            claims,
            settings,
            network,
            advertisement,
            enabled: false,
            applied: None,
            last_error: None,
            started: None,
        }
    }

    /// Checks now, then every [`ADVERTISING_CHECK_INTERVAL`], for as long as the pond runs.
    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(ADVERTISING_CHECK_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            self.check().await;
        }
    }

    /// One check, applied to the peripheral only when the decision changed or the last one failed.
    pub async fn check(&mut self) -> Decision {
        match self.settings.get().await {
            Ok(settings) => self.enabled = settings.ble_setup_enabled,
            Err(error) => {
                tracing::warn!(%error, "settings unreadable; the setup channel keeps its last state")
            }
        }
        self.channel.set_accepting(self.enabled);
        let claimed = match self.claims.claim_state().await {
            Ok(state) => state.is_claimed(),
            Err(error) => {
                // As the session reads it: claimed narrows when the pond shows itself.
                tracing::warn!(%error, "claim state unreadable; advertising as if claimed");
                true
            }
        };
        let has_usable_network = self.network.has_usable_network();
        let advertise = self.enabled && should_advertise(claimed, has_usable_network);
        let decision = Decision {
            enabled: self.enabled,
            claimed,
            has_usable_network,
            advertise,
        };
        if self.enabled && self.started.is_none() {
            self.started = Some(self.start().await);
        }
        if self.started == Some(Ok(())) && self.applied != Some(advertise) {
            self.apply(decision).await;
        }
        decision
    }

    /// Once: a peripheral that cannot start here (no adapter, no radio) will not start later.
    async fn start(&self) -> Result<(), ()> {
        match self.peripheral.start(Arc::new(self.channel.clone())).await {
            Ok(()) => {
                tracing::info!(
                    kind = "ble_setup_started",
                    "the setup channel's peripheral is starting"
                );
                Ok(())
            }
            Err(error) => {
                tracing::warn!(
                    kind = "ble_unavailable",
                    %error,
                    "BLE setup is unavailable on this host; the pond carries on without it"
                );
                Err(())
            }
        }
    }

    async fn apply(&mut self, decision: Decision) {
        let wanted = decision.advertise.then(|| self.advertisement.clone());
        match self.peripheral.advertise(wanted).await {
            Ok(()) => {
                self.applied = Some(decision.advertise);
                self.last_error = None;
                tracing::info!(
                    kind = "ble_advertising",
                    advertising = decision.advertise,
                    enabled = decision.enabled,
                    claimed = decision.claimed,
                    has_usable_network = decision.has_usable_network,
                    "setup channel advertising changed"
                );
            }
            Err(error) => {
                self.applied = None;
                let error = error.to_string();
                // Once per distinct failure: a pond without Bluetooth would otherwise log every 10 s.
                if self.last_error.as_deref() != Some(error.as_str()) {
                    tracing::warn!(
                        kind = "ble_advertise_failed",
                        advertising = decision.advertise,
                        %error,
                        "the setup channel could not change advertising; retrying on the next check"
                    );
                    self.last_error = Some(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use pond_core::network::mocks::mock_setup_code_store::MockSetupCodeStore;
    use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;

    use super::*;
    use crate::ble::{BleError, PeripheralStatus};
    use crate::test_support::test_pond;

    #[derive(Default)]
    struct RecordingPeripheral {
        calls: Mutex<Vec<Option<String>>>,
        starts: AtomicUsize,
        failing: AtomicBool,
        unsupported: AtomicBool,
    }

    #[async_trait]
    impl BlePeripheral for RecordingPeripheral {
        async fn start(&self, _: Arc<SetupChannel>) -> Result<(), BleError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            if self.unsupported.load(Ordering::SeqCst) {
                return Err(BleError::Unavailable("no adapter".into()));
            }
            Ok(())
        }
        async fn advertise(&self, advertisement: Option<Advertisement>) -> Result<(), BleError> {
            self.calls
                .lock()
                .unwrap()
                .push(advertisement.map(|a| a.local_name));
            if self.failing.load(Ordering::SeqCst) {
                return Err(BleError::Unavailable("Bluetooth is off".into()));
            }
            Ok(())
        }
        async fn shutdown(&self) {}
        fn status(&self) -> PeripheralStatus {
            PeripheralStatus::Serving
        }
    }

    #[derive(Default)]
    struct Network(AtomicBool);

    impl NetworkProbe for Network {
        fn has_usable_network(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct Rig {
        controller: AdvertisingController,
        peripheral: Arc<RecordingPeripheral>,
        channel: SetupChannel,
        store: Arc<MockSetupCodeStore>,
        settings: Arc<MockSettingsRepository>,
        network: Arc<Network>,
    }

    impl Rig {
        fn calls(&self) -> Vec<Option<String>> {
            self.peripheral.calls.lock().unwrap().clone()
        }
    }

    fn rig() -> Rig {
        let pond = test_pond();
        let peripheral = Arc::new(RecordingPeripheral::default());
        let network = Arc::new(Network::default());
        Rig {
            controller: AdvertisingController::new(
                peripheral.clone(),
                pond.channel.clone(),
                pond.claims.clone(),
                pond.settings.clone(),
                network.clone(),
            ),
            peripheral,
            channel: pond.channel,
            store: pond.store,
            settings: pond.settings,
            network,
        }
    }
    #[tokio::test]
    async fn it_advertises_the_cards_name_while_unclaimed_or_offline_and_only_on_change() {
        let mut rig = rig();
        let first = rig.controller.check().await;
        assert!(first.advertise && !first.claimed && !first.has_usable_network);
        assert_eq!(rig.calls(), [Some("Pond 7f3a".to_string())]);

        rig.network.0.store(true, Ordering::SeqCst);
        assert!(
            rig.controller.check().await.advertise,
            "unclaimed on Ethernet"
        );
        assert_eq!(rig.calls().len(), 1, "no call when nothing changed");

        rig.store.set_claimed_at(Some(chrono::Utc::now()));
        assert!(!rig.controller.check().await.advertise);
        rig.network.0.store(false, Ordering::SeqCst);
        assert!(
            rig.controller.check().await.advertise,
            "claimed but offline"
        );
        assert_eq!(
            rig.calls(),
            [
                Some("Pond 7f3a".to_string()),
                None,
                Some("Pond 7f3a".to_string())
            ]
        );
    }

    #[tokio::test]
    async fn the_household_switch_silences_the_radio_and_refuses_new_phones() {
        let mut rig = rig();
        rig.controller.check().await;
        let open = rig.channel.open().expect("a phone connects while on");

        rig.settings
            .set_key("ble_setup_enabled", "false".into())
            .await
            .unwrap();
        let off = rig.controller.check().await;
        assert!(!off.enabled && !off.advertise);
        assert_eq!(rig.calls().last(), Some(&None));
        assert!(!rig.channel.is_accepting());
        assert!(
            rig.channel.is_session_open(),
            "the open session is left alone"
        );
        drop(open);

        rig.settings
            .set_key("ble_setup_enabled", "true".into())
            .await
            .unwrap();
        assert!(rig.controller.check().await.advertise);
        assert!(rig.channel.is_accepting());
    }

    #[tokio::test]
    async fn a_failed_change_is_retried_on_the_next_check() {
        let mut rig = rig();
        rig.peripheral.failing.store(true, Ordering::SeqCst);
        rig.controller.check().await;
        rig.controller.check().await;
        assert_eq!(rig.calls().len(), 2, "retried while it fails");

        rig.peripheral.failing.store(false, Ordering::SeqCst);
        rig.controller.check().await;
        rig.controller.check().await;
        assert_eq!(rig.calls().len(), 3, "then settles");
    }

    #[tokio::test]
    async fn a_pond_switched_off_never_touches_the_radio_and_starts_it_once_switched_on() {
        let mut rig = rig();
        rig.settings
            .set_key("ble_setup_enabled", "false".into())
            .await
            .unwrap();
        rig.controller.check().await;
        rig.controller.check().await;
        assert_eq!(rig.peripheral.starts.load(Ordering::SeqCst), 0);
        assert!(rig.calls().is_empty());

        rig.settings
            .set_key("ble_setup_enabled", "true".into())
            .await
            .unwrap();
        rig.controller.check().await;
        rig.controller.check().await;
        assert_eq!(rig.peripheral.starts.load(Ordering::SeqCst), 1);
        assert_eq!(rig.calls(), [Some("Pond 7f3a".to_string())]);
    }

    #[tokio::test]
    async fn a_host_without_an_adapter_is_tried_once_and_left_alone() {
        let mut rig = rig();
        rig.peripheral.unsupported.store(true, Ordering::SeqCst);
        for _ in 0..3 {
            rig.controller.check().await;
        }
        assert_eq!(rig.peripheral.starts.load(Ordering::SeqCst), 1);
        assert!(rig.calls().is_empty());
    }
}
