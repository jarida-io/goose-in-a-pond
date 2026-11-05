//! `GET /api/v1/rooms/{id}/devices` — a room's devices without a client having to fetch every
//! device and filter by `room_id` itself.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
use pond_api::{build_router, AppState};
use pond_core::security::ports::handshake::Handshake;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Harness {
    lan: axum::Router,
    loopback: axum::Router,
    handshake: Arc<pond_infra::sqlite_handshake::SqliteHandshakeAdapter>,
    _dir: tempfile::TempDir,
}

async fn fresh_pond() -> Harness {
    let pond_api::test_support::TestState {
        state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    let state: Arc<AppState> = Arc::new(state);
    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let loopback = build_router(state.clone(), dist.clone())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    let lan = build_router(state, dist).layer(MockConnectInfo(SocketAddr::from((
        [192, 168, 1, 50],
        40_000,
    ))));
    Harness {
        lan,
        loopback,
        handshake,
        _dir: dir,
    }
}

async fn paired(h: &Harness) -> String {
    let code = h.handshake.issue_pairing_code().await.unwrap().code;
    let (status, body) = pair_phone(&h.loopback, &code, "phone", TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session_token"].as_str().unwrap().to_string()
}

async fn send(
    router: &axum::Router,
    method: Method,
    path: &str,
    body: Option<Value>,
    bearer: &str,
) -> (StatusCode, Value) {
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .body(Body::from(body))
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

async fn create_room(h: &Harness, token: &str, name: &str) -> String {
    let (status, body) = send(
        &h.lan,
        Method::POST,
        "/api/v1/rooms",
        Some(json!({"name": name})),
        token,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

async fn register_device(h: &Harness, token: &str, name: &str, room_id: Option<&str>) -> String {
    let mut body = json!({"name": name, "device_type": "light", "capabilities": []});
    if let Some(room_id) = room_id {
        body["room_id"] = json!(room_id);
    }
    let (status, body) = send(&h.lan, Method::POST, "/api/v1/devices", Some(body), token).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn unknown_room_404s() {
    let h = fresh_pond().await;
    let token = paired(&h).await;

    let (status, body) = send(
        &h.lan,
        Method::GET,
        "/api/v1/rooms/does-not-exist/devices",
        None,
        &token,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "room_not_found");
}

#[tokio::test]
async fn an_empty_room_has_no_devices() {
    let h = fresh_pond().await;
    let token = paired(&h).await;
    let kitchen = create_room(&h, &token, "Kitchen").await;

    let (status, body) = send(
        &h.lan,
        Method::GET,
        &format!("/api/v1/rooms/{kitchen}/devices"),
        None,
        &token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["devices"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn only_devices_placed_in_the_room_are_listed() {
    let h = fresh_pond().await;
    let token = paired(&h).await;
    let kitchen = create_room(&h, &token, "Kitchen").await;
    let bedroom = create_room(&h, &token, "Bedroom").await;

    let lamp = register_device(&h, &token, "Lamp", Some(&kitchen)).await;
    register_device(&h, &token, "Nightlight", Some(&bedroom)).await;
    register_device(&h, &token, "Unsorted", None).await;

    let (status, body) = send(
        &h.lan,
        Method::GET,
        &format!("/api/v1/rooms/{kitchen}/devices"),
        None,
        &token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let devices = body["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 1, "{body}");
    assert_eq!(devices[0]["id"], lamp);
    assert_eq!(devices[0]["room_id"], kitchen);
}

#[tokio::test]
async fn a_room_starts_guest_inaccessible_and_can_be_toggled() {
    let h = fresh_pond().await;
    let token = paired(&h).await;
    let kitchen = create_room(&h, &token, "Kitchen").await;

    let (status, body) = send(
        &h.lan,
        Method::GET,
        &format!("/api/v1/rooms/{kitchen}"),
        None,
        &token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["guest_accessible"], false);

    let (status, body) = send(
        &h.lan,
        Method::PUT,
        &format!("/api/v1/rooms/{kitchen}/guest-access"),
        Some(json!({"accessible": true})),
        &token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["guest_accessible"], true);

    let (status, body) = send(
        &h.lan,
        Method::GET,
        "/api/v1/rooms",
        None,
        &token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rooms = body["rooms"].as_array().unwrap();
    assert_eq!(rooms[0]["guest_accessible"], true);
}
