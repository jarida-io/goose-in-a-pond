//! A path under `/api/` that no route serves answers with a JSON 404, never the dashboard: a
//! client reading the dashboard's `200` would believe its write succeeded.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

async fn paired_state() -> (pond_api::AppState, tempfile::TempDir) {
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
    (state, built.dir)
}

async fn router() -> (axum::Router, tempfile::TempDir, tempfile::TempDir) {
    let (state, dir) = paired_state().await;
    let web = tempfile::tempdir().unwrap();
    std::fs::write(web.path().join("index.html"), "<html>the dashboard</html>").unwrap();
    let router = build_router(Arc::new(state), web.path().to_path_buf());
    (router, web, dir)
}

async fn send(router: &axum::Router, method: Method, uri: &str) -> (StatusCode, String, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let kind = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, kind, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn an_unknown_api_path_is_a_json_404_whatever_the_method() {
    let (router, _web, _dir) = router().await;
    for uri in [
        "/api/v1/no-such-route",
        "/api/v1/agents/no-such-agent/thing",
        "/api/v2/anything",
        "/api",
    ] {
        for method in [Method::GET, Method::PUT, Method::POST, Method::DELETE] {
            let (status, kind, body) = send(&router, method.clone(), uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {body}");
            assert!(
                kind.starts_with("application/json"),
                "{method} {uri}: {kind}"
            );
            let body: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                body,
                serde_json::json!({"error": "not_found"}),
                "{method} {uri}"
            );
        }
    }
}

#[tokio::test]
async fn a_known_path_with_the_wrong_method_is_still_405() {
    let (router, _web, _dir) = router().await;
    let (status, _, body) = send(&router, Method::DELETE, "/api/v1/health").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{body}");
}

#[tokio::test]
async fn everything_outside_the_api_is_still_the_dashboard() {
    let (router, _web, _dir) = router().await;
    for uri in ["/", "/chat", "/settings/voice", "/apiary"] {
        let (status, kind, _) = send(&router, Method::GET, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(kind.starts_with("text/html"), "{uri}: {kind}");
    }
}

#[tokio::test]
async fn the_companion_listener_answers_unknown_paths_the_same_way() {
    let (state, _dir) = paired_state().await;
    let router = pond_api::build_companion_router(Arc::new(state));
    let (status, kind, body) = send(&router, Method::PUT, "/api/v1/no-such-route").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(kind.starts_with("application/json"), "{kind}");
}
