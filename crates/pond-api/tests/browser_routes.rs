//! `/browser` over HTTP: unavailable until a browser port is installed, then its status.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::shared::ports::browser::{self, Browser, Install, Status};
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use serde_json::Value;
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

struct NoneYet;

#[async_trait]
impl Browser for NoneYet {
    fn status(&self) -> Status {
        Status {
            enabled: true,
            browser: None,
            own: false,
            free_mb: Some(498),
            needs_free_mb: 900,
            ready: false,
            reason: Some("no_browser"),
            can_download: false,
            install: Install::Idle,
        }
    }

    async fn install(&self) -> Result<(), String> {
        Err("not in a test".to_string())
    }

    async fn uninstall(&self) -> bool {
        false
    }
}

async fn call(router: &axum::Router, method: Method, uri: &str) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            51_000,
        ))));
    let response = router.clone().oneshot(request).await.unwrap();
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
async fn the_browser_reports_why_a_page_could_not_be_rendered_and_refuses_a_download_it_cannot_do()
{
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let device_id = state
        .device_registry
        .register(RegisterDeviceRequest {
            id: None,
            name: "Phone".into(),
            device_type: "gotg".into(),
            hostname: None,
            capabilities: vec![],
            room_id: None,
        })
        .await
        .unwrap()
        .id;
    state.handshake = Arc::new(DeviceHandshake(device_id));
    let router = build_router(
        Arc::new(state),
        std::path::PathBuf::from("pond-desktop/dist"),
    );
    browser::install(Arc::new(NoneYet));

    let (status, body) = call(&router, Method::GET, "/api/v1/browser").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ready"], false);
    assert_eq!(body["reason"], "no_browser");
    assert_eq!(body["free_mb"], 498);
    assert_eq!(body["install"]["state"], "idle");

    let (status, body) = call(&router, Method::POST, "/api/v1/browser/install").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}
