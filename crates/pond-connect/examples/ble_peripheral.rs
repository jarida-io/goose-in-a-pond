//! Serves the setup channel over this host's BLE peripheral for an unclaimed, throwaway pond
//! (`Pond 7f3a`, setup code `7992-739-8713`), so the radio and a phone's whole claim can be
//! checked without touching a real pond's claim. Its handshake runs on a temporary database.
//!
//! `--offline` reports no network, so a phone shows its Wi-Fi list. A join is refused unless
//! `--allow-join` is given: on a headless board, a wrong join cuts off the only way in.
//!
//! ```text
//! cargo run -p pond-connect --example ble_peripheral -- [seconds, default 300] [--offline] [--allow-join]
//! ```

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pond_connect::ble::{self, Advertisement};
use pond_connect::setup_channel::{SetupChannel, SetupChannelConfig, SetupChannelContext};
use pond_core::network::domain::capability::Availability;
use pond_core::network::domain::wifi::{
    JoinedNetwork, NetworkStatus, WifiCapabilities, WifiJoinError, WifiNetwork, WifiPassphrase,
    WifiScanError,
};
use pond_core::network::mocks::mock_setup_code_store::MockSetupCodeStore;
use pond_core::network::ports::wifi_station::WifiStation;
use pond_core::network::services::setup_claim::SetupClaimService;
use pond_core::network::services::spoken_code::SpokenCodeService;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_infra::db::Database;
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;

/// The host's real station, reporting no network when asked and joining only when allowed.
struct GuardedStation {
    inner: Arc<dyn WifiStation>,
    offline: bool,
    allow_join: bool,
}

#[async_trait]
impl WifiStation for GuardedStation {
    async fn capabilities(&self) -> WifiCapabilities {
        let real = self.inner.capabilities().await;
        WifiCapabilities {
            scan: real.scan,
            join: if self.allow_join {
                real.join
            } else {
                Availability::unavailable("this example joins only with --allow-join")
            },
        }
    }

    async fn scan(&self) -> Result<Vec<WifiNetwork>, WifiScanError> {
        self.inner.scan().await
    }

    async fn join(
        &self,
        ssid: &str,
        passphrase: Option<&WifiPassphrase>,
        hidden: bool,
    ) -> Result<JoinedNetwork, WifiJoinError> {
        if !self.allow_join {
            return Err(WifiJoinError::NotPermitted);
        }
        self.inner.join(ssid, passphrase, hidden).await
    }

    async fn status(&self) -> NetworkStatus {
        if self.offline {
            return NetworkStatus::disconnected();
        }
        self.inner.status().await
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seconds: u64 = args.iter().find_map(|arg| arg.parse().ok()).unwrap_or(300);
    let offline = args.iter().any(|arg| arg == "--offline");
    let allow_join = args.iter().any(|arg| arg == "--allow-join");

    let store = MockSetupCodeStore::unclaimed();
    let card = store
        .record()
        .expect("the mock store holds a record")
        .card();
    let claims = Arc::new(SetupClaimService::new(Arc::new(store)));
    let scratch = tempfile::tempdir().expect("a temporary directory");
    let db = Database::init(scratch.path())
        .await
        .expect("the temporary database");
    // Any well-formed pin will do: the phone binds the one `hello` reports, and no TLS follows.
    let pin = format!("sha256/{}=", "A".repeat(43));
    let handshake = SqliteHandshakeAdapter::new(db.system.clone(), Some(pin.clone()))
        .with_setup_claims(claims.clone());
    println!(
        "card {} setup code {} offline={offline} allow_join={allow_join}",
        ble::local_name(MockSetupCodeStore::DISCRIMINATOR),
        card.code.display()
    );

    let channel = SetupChannel::new(
        SetupChannelContext::new(
            SetupChannelConfig {
                discriminator: MockSetupCodeStore::DISCRIMINATOR,
                pin,
                https_port: 8443,
                hostname: "pond-example".into(),
            },
            Arc::new(handshake),
            Arc::new(SpokenCodeService::default()),
            Arc::new(MockSettingsRepository::new()),
            Arc::new(MockProfileRepository::new()),
        )
        .with_claims(claims)
        .with_wifi(Arc::new(GuardedStation {
            inner: pond_connect::wifi::platform(),
            offline,
            allow_join,
        })),
    );
    let peripheral = ble::platform();
    if let Err(error) = peripheral.start(Arc::new(channel)).await {
        eprintln!("start: {error}");
    }
    let advertisement = Advertisement {
        local_name: ble::local_name(MockSetupCodeStore::DISCRIMINATOR),
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let mut last = None;
    while tokio::time::Instant::now() < deadline {
        // Asked again each round, as the server's controller does after a failure.
        let applied = peripheral.advertise(Some(advertisement.clone())).await;
        let status = peripheral.status();
        let line = format!("{status:?} {applied:?}");
        if last.as_ref() != Some(&line) {
            println!("{line}");
            last = Some(line);
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    peripheral.shutdown().await;
    println!("{:?}", peripheral.status());
}
