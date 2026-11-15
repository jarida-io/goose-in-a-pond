//! A pond on Wi-Fi with a station, a wired link and a setup channel behind the connections
//! routes, and the household phones paired to it.
// Each test binary that includes this uses only part of it.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use axum::{Extension, Router};
use pond_api::connections::Connections;
use pond_api::host_guard::{HostCredential, CREDENTIAL_HEADER};
use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
use pond_api::AppState;
use pond_core::network::domain::peripheral::PeripheralStatus;
use pond_core::network::domain::wifi::{WifiCapabilities, WifiSecurity};
use pond_core::network::domain::wired::{LinkState, WiredInterface};
use pond_core::network::mocks::mock_setup_code_store::MockSetupCodeStore;
use pond_core::network::mocks::mock_wifi_station::{sample_network, MockWifiStation};
use pond_core::network::mocks::mock_wired_interfaces::MockWiredInterfaces;
use pond_core::network::ports::setup_radios::SetupRadios;
use pond_core::network::services::setup_claim::SetupClaimService;
use pond_core::network::services::wifi_connections::WifiConnections;
use pond_core::security::ports::handshake::Handshake;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
use serde_json::Value;
use tower::ServiceExt;

pub const PASSPHRASE: &str = "otter-lantern-quietly-91";

/// Pairing's verify limiter is per address and process-wide, so each pond pairs from its own
/// loopback address.
pub fn host_peer() -> SocketAddr {
    static NEXT: AtomicU8 = AtomicU8::new(1);
    SocketAddr::from(([127, 0, 0, NEXT.fetch_add(1, Ordering::Relaxed)], 40_000))
}

pub struct Radios;

#[async_trait]
impl SetupRadios for Radios {
    fn ble(&self) -> PeripheralStatus {
        PeripheralStatus::Advertising
    }
    fn session_open(&self) -> bool {
        false
    }
    fn advertising_as(&self) -> Option<String> {
        Some("Pond 7f3a".into())
    }
    async fn wifi(&self) -> WifiCapabilities {
        WifiCapabilities::available()
    }
}

pub struct Pond {
    /// The HTTPS companion listener, reached from a phone on the LAN.
    pub lan: Router,
    /// The loopback dashboard, which holds the host credential.
    pub host: Router,
    /// The companion listener of a server wired with no station and no setup channel.
    pub bare: Router,
    pub credential: HostCredential,
    pub station: Arc<MockWifiStation>,
    pub handshake: Arc<SqliteHandshakeAdapter>,
    pub state: Arc<AppState>,
    _dir: tempfile::TempDir,
}

/// A pond mid-owner-setup, on Wi-Fi "Home", unclaimed until its first phone pairs.
pub async fn pond() -> Pond {
    let pond_api::test_support::TestState { mut state, dir, .. } =
        pond_api::test_support::app_state().await;
    state.skip_onboarding = false;
    let claims = Arc::new(SetupClaimService::new(Arc::new(
        MockSetupCodeStore::unclaimed(),
    )));
    let handshake = Arc::new(
        SqliteHandshakeAdapter::new(state.db.system.clone(), Some(TEST_SPKI_PIN.to_string()))
            .with_setup_claims(claims.clone()),
    );
    state.handshake = handshake.clone();
    state.setup_claims = Some(claims);

    let station = Arc::new(
        MockWifiStation::new()
            .with_network(
                sample_network("Home", WifiSecurity::Wpa2, 70),
                Some(PASSPHRASE),
            )
            .with_network(sample_network("Cafe", WifiSecurity::Open, 40), None)
            .with_network(
                sample_network("Office", WifiSecurity::Enterprise, 55),
                Some(PASSPHRASE),
            )
            .with_saved("Attic")
            .with_saved("Flat 2/B"),
    );
    station.connect_to("Home");
    let wired = Arc::new(MockWiredInterfaces::new(vec![
        WiredInterface {
            interface: "eth0".into(),
            link: LinkState::Up,
            address: Some("192.168.1.20".parse().unwrap()),
            speed_mbps: Some(1000),
        },
        WiredInterface {
            interface: "eth1".into(),
            link: LinkState::Down,
            address: None,
            speed_mbps: None,
        },
    ]));
    let connections = Arc::new(Connections::new(
        WifiConnections::new(station.clone()),
        wired,
        "nano",
        4443,
    ));
    let state = Arc::new(state);
    let credential = HostCredential::generate();
    let radios: Arc<dyn SetupRadios> = Arc::new(Radios);
    let lan_peer = SocketAddr::from(([192, 168, 1, 50], 40_000));
    let lan = pond_api::build_companion_router(state.clone())
        .layer(Extension(connections.clone()))
        .layer(Extension(radios))
        .layer(MockConnectInfo(lan_peer));
    let host = pond_api::build_router(state.clone(), "pond-desktop/dist".into())
        .layer(Extension(connections))
        .layer(Extension(credential.clone()))
        .layer(MockConnectInfo(host_peer()));
    let bare = pond_api::build_companion_router(state.clone()).layer(MockConnectInfo(lan_peer));
    Pond {
        lan,
        host,
        bare,
        credential,
        station,
        handshake,
        state,
        _dir: dir,
    }
}

/// Pairs over loopback; the first phone paired claims the pond. The code is host-approved, as
/// `pond-server pairing` mints it, so the lock a member's phone switches on never holds the next.
pub async fn paired(pond: &Pond, client_id: &str, profile_id: Option<&str>) -> String {
    let code = pond
        .handshake
        .issue_pairing_code_for(profile_id)
        .await
        .unwrap()
        .code;
    pond.handshake
        .mark_pairing_code_approved(&code)
        .await
        .unwrap();
    let (status, body) = pair_phone(&pond.host, &code, client_id, TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session_token"].as_str().unwrap().to_string()
}

pub struct Household {
    pub claimant: String,
    pub member: String,
    pub guest: String,
}

/// The phone that claimed the pond, a member's phone and a guest's, in that pairing order.
pub async fn household(pond: &Pond) -> Household {
    let claimant = paired(pond, "install-owner", None).await;
    let amina = pond
        .state
        .profile_repo
        .create(CreateProfileRequest {
            display_name: "Amina".into(),
            avatar_emoji: "A".into(),
        })
        .await
        .unwrap();
    let member = paired(pond, "install-amina", Some(&amina.id)).await;
    let guest = paired(pond, "install-guest", None).await;
    Household {
        claimant,
        member,
        guest,
    }
}

pub async fn send(
    router: &Router,
    method: Method,
    path: &str,
    body: Option<&str>,
    bearer: Option<&str>,
    host_credential: Option<&HostCredential>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    if let Some(credential) = host_credential {
        request = request.header(CREDENTIAL_HEADER, credential.as_str());
    }
    let response = router
        .clone()
        .oneshot(
            request
                .body(Body::from(body.unwrap_or_default().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

pub async fn get(router: &Router, path: &str, token: &str) -> (StatusCode, Value) {
    send(router, Method::GET, path, None, Some(token), None).await
}

pub async fn join(router: &Router, token: &str, body: &str) -> (StatusCode, Value) {
    send(
        router,
        Method::POST,
        "/api/v1/connections/wifi/join",
        Some(body),
        Some(token),
        None,
    )
    .await
}

pub async fn forget(router: &Router, token: &str, path: &str) -> (StatusCode, Value) {
    send(router, Method::DELETE, path, None, Some(token), None).await
}
