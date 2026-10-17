//! `GET /api/v1/network/status`: the setup channel's radios, for a paired device or the host only.
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Request, StatusCode};
use axum::Extension;
use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
use pond_core::network::domain::capability::Availability;
use pond_core::network::domain::peripheral::PeripheralStatus;
use pond_core::network::domain::wifi::WifiCapabilities;
use pond_core::network::ports::setup_radios::SetupRadios;
use pond_core::security::ports::handshake::Handshake;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Radios;

#[async_trait]
impl SetupRadios for Radios {
    fn ble(&self) -> PeripheralStatus {
        PeripheralStatus::unavailable("Bluetooth is turned off on this Mac")
    }
    fn session_open(&self) -> bool {
        true
    }
    async fn wifi(&self) -> WifiCapabilities {
        WifiCapabilities {
            scan: Availability::unavailable("no Location permission"),
            join: Availability::Available,
        }
    }
}

async fn get(router: &axum::Router, bearer: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::get("/api/v1/network/status");
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
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

#[tokio::test]
async fn a_paired_device_reads_the_radios_and_why_each_is_off() {
    let pond_api::test_support::TestState {
        state,
        handshake,
        dir: _dir,
    } = pond_api::test_support::app_state().await;
    let state = Arc::new(state);
    let lan = SocketAddr::from(([192, 168, 1, 50], 40_000));
    let bare = pond_api::build_companion_router(state.clone()).layer(MockConnectInfo(lan));
    let wired = pond_api::build_companion_router(state)
        .layer(Extension(Arc::new(Radios) as Arc<dyn SetupRadios>))
        .layer(MockConnectInfo(lan));

    let (status, _) = get(&wired, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "never tokenless");

    let code = handshake.issue_pairing_code().await.unwrap().code;
    let (status, paired) = pair_phone(&wired, &code, "status-phone", TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{paired}");
    let token = paired["session_token"].as_str().unwrap();

    let (status, body) = get(&wired, Some(token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "ble": {"enabled": true, "state": "unavailable",
                    "reason": "Bluetooth is turned off on this Mac", "session_open": true},
            "wifi": {"scan": {"available": false, "reason": "no Location permission"},
                     "join": {"available": true, "reason": null}},
        })
    );

    let (status, body) = get(&bare, Some(token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ble"]["state"], "unavailable");
    assert_eq!(
        body["ble"]["reason"],
        "this pond is not running the setup channel"
    );
    assert_eq!(body["wifi"]["join"]["available"], false);
}
