//! The household's notification history (`GET /notifications/history`,
//! `POST /notifications/history/read`), wired as `main.rs` wires it: the real queue, recipients,
//! attribution and history behind one sender. Each device reads and marks only what it may read,
//! named by its token; a notice is listed once under its household id however many copies went
//! out; a member's notice never reaches another member's device; live status is never listed.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
use pond_api::{build_router, AppState};
use pond_core::mcp::ports::notification::{Notification, NotificationSender};
use pond_core::security::ports::handshake::Handshake;
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_infra::broadcast_notification_sender::BroadcastNotificationSender;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use pond_infra::sqlite_notification_history::SqliteNotificationHistory;
use pond_infra::sqlite_notification_queue::SqliteNotificationQueue;
use pond_infra::sqlite_notification_recipients::SqliteNotificationRecipients;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Harness {
    loopback: axum::Router,
    lan: axum::Router,
    handshake: Arc<pond_infra::sqlite_handshake::SqliteHandshakeAdapter>,
    sender: Arc<BroadcastNotificationSender>,
    attribution: Arc<SqliteDeviceAttribution>,
    pool: sqlx::SqlitePool,
    _dir: tempfile::TempDir,
}

async fn pond() -> Harness {
    let pond_api::test_support::TestState {
        mut state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    let pool = state.db.system.clone();
    let queue = Arc::new(SqliteNotificationQueue::new(pool.clone()));
    let history = Arc::new(SqliteNotificationHistory::new(pool.clone()));
    let attribution = Arc::new(SqliteDeviceAttribution::new(pool.clone()));
    let (tx, _) = tokio::sync::broadcast::channel::<Notification>(64);
    let sender = Arc::new(
        BroadcastNotificationSender::new(tx.clone(), queue.clone(), None)
            .with_household_recipients(Arc::new(SqliteNotificationRecipients::new(pool.clone())))
            .with_device_attribution(attribution.clone())
            .with_history(history.clone()),
    );
    state.notification_tx = tx;
    state.notification_queue = Some(queue);
    state.notification_history = Some(history);
    state.notification_sender = Some(sender.clone());
    let state: Arc<AppState> = Arc::new(state);
    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let loopback = build_router(state.clone(), dist.clone())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    let lan = build_router(state, dist).layer(MockConnectInfo(SocketAddr::from((
        [192, 168, 1, 50],
        40_000,
    ))));
    Harness {
        loopback,
        lan,
        handshake,
        sender,
        attribution,
        pool,
        _dir: dir,
    }
}

struct Device {
    token: String,
    id: String,
}

async fn paired(h: &Harness, client_id: &str) -> Device {
    let code = h.handshake.issue_pairing_code().await.unwrap().code;
    let (status, body) = pair_phone(&h.loopback, &code, client_id, TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["session_token"].as_str().unwrap().to_string();
    let (status, me) = call(
        &h.lan,
        Method::GET,
        "/api/v1/devices/self",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    Device {
        token,
        id: me["device_id"].as_str().unwrap().to_string(),
    }
}

/// `device` belongs to the member `profile`.
async fn attribute(h: &Harness, device: &Device, profile: &str) {
    sqlx::query("INSERT OR IGNORE INTO profiles (id, display_name) VALUES (?, ?)")
        .bind(profile)
        .bind(profile)
        .execute(&h.pool)
        .await
        .unwrap();
    h.attribution
        .set_device_profile(&device.id, Some(profile))
        .await
        .unwrap();
}

async fn call(
    router: &axum::Router,
    method: Method,
    path: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let request = match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
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

fn notice(id: &str) -> Notification {
    Notification {
        id: id.into(),
        target: "broadcast".into(),
        category: "alert".into(),
        title: "The Pond is running out of space".into(),
        body: "Only 1 GB is free.".into(),
        timestamp: "2026-10-28T07:00:00Z".into(),
        data: Some(json!({"open": "models", "kind": "disk_low"})),
    }
}

/// The history as `device` reads it; asserts the status before reading the body.
async fn history(h: &Harness, device: &Device) -> Value {
    let (status, body) = call(
        &h.lan,
        Method::GET,
        "/api/v1/notifications/history",
        Some(&device.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// The ids `device` may read, minus the pairing notices every pairing raises.
async fn ids(h: &Harness, device: &Device) -> Vec<String> {
    history(h, device).await["notifications"]
        .as_array()
        .expect("a list")
        .iter()
        .filter(|n| n["data"]["open"] != "devices")
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn the_history_needs_a_paired_device_s_token() {
    let h = pond().await;
    for router in [&h.loopback, &h.lan] {
        let (status, _) = call(
            router,
            Method::GET,
            "/api/v1/notifications/history",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "no token, no history");
        let (status, _) = call(
            router,
            Method::POST,
            "/api/v1/notifications/history/read",
            None,
            Some(json!({"ids": []})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "no token, no read marks");
    }
    let (status, _) = call(
        &h.lan,
        Method::GET,
        "/api/v1/notifications/history",
        Some("not-a-token"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_household_notice_is_listed_once_under_its_own_id_with_its_payload() {
    let h = pond().await;
    let a = paired(&h, "phone-a").await;
    let b = paired(&h, "phone-b").await;

    h.sender.broadcast(notice("disk-1")).await.unwrap();

    let body = history(&h, &a).await;
    assert_eq!(body["device_id"], json!(a.id), "the caller's own device");
    let entry = body["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "disk-1")
        .expect("the notice is listed")
        .clone();
    assert_eq!(
        entry,
        json!({
            "id": "disk-1",
            "category": "alert",
            "title": "The Pond is running out of space",
            "body": "Only 1 GB is free.",
            "timestamp": "2026-10-28T07:00:00Z",
            "data": {"open": "models", "kind": "disk_low"},
            "read": false,
        }),
        "under the household id, never a phone's own copy's id"
    );
    assert_eq!(
        ids(&h, &a).await,
        vec!["disk-1"],
        "one entry, not one per phone copy"
    );
    assert_eq!(ids(&h, &b).await, vec!["disk-1"]);
}

#[tokio::test]
async fn a_member_s_notice_is_never_listed_to_another_member_s_device() {
    let h = pond().await;
    let liz = paired(&h, "phone-liz").await;
    let jerry = paired(&h, "phone-jerry").await;
    attribute(&h, &liz, "liz").await;
    attribute(&h, &jerry, "jerry").await;

    let report = h.sender.send_to_profile("liz", notice("proposal-1")).await;
    assert_eq!(report.queued, vec![liz.id.clone()]);

    assert_eq!(ids(&h, &liz).await, vec!["proposal-1"]);
    assert!(
        ids(&h, &jerry).await.is_empty(),
        "another member's device never lists it"
    );

    let (status, body) = call(
        &h.lan,
        Method::POST,
        "/api/v1/notifications/history/read",
        Some(&jerry.token),
        Some(json!({"ids": ["proposal-1"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"marked": 0}),
        "a device cannot mark a notice it may not read, and is not told it exists"
    );
}

#[tokio::test]
async fn the_device_that_caused_a_notice_does_not_list_it() {
    let h = pond().await;
    let a = paired(&h, "phone-a").await;
    let b = paired(&h, "phone-b").await;

    h.sender
        .broadcast_except(notice("removed-1"), &a.id)
        .await
        .unwrap();

    assert!(ids(&h, &a).await.is_empty());
    assert_eq!(ids(&h, &b).await, vec!["removed-1"]);
}

#[tokio::test]
async fn a_live_status_is_never_listed() {
    let h = pond().await;
    let a = paired(&h, "phone-a").await;

    let mut warming = notice("warmup-1");
    warming.data = Some(json!({"kind": "pond_warmup"}));
    h.sender.publish_live(warming).await.unwrap();

    assert!(ids(&h, &a).await.is_empty());
}

#[tokio::test]
async fn read_state_is_the_caller_s_own() {
    let h = pond().await;
    let a = paired(&h, "phone-a").await;
    let b = paired(&h, "phone-b").await;
    h.sender.broadcast(notice("disk-1")).await.unwrap();

    let (status, body) = call(
        &h.lan,
        Method::POST,
        "/api/v1/notifications/history/read",
        Some(&a.token),
        Some(json!({"ids": ["disk-1", "no-such-notice"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"marked": 1}));

    let read = |body: Value| {
        body["notifications"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "disk-1")
            .unwrap()["read"]
            .clone()
    };
    assert_eq!(read(history(&h, &a).await), json!(true));
    assert_eq!(
        read(history(&h, &b).await),
        json!(false),
        "marking on one device leaves the other's unread"
    );

    let too_many: Vec<String> = (0..501).map(|i| format!("n{i}")).collect();
    let (status, _) = call(
        &h.lan,
        Method::POST,
        "/api/v1/notifications/history/read",
        Some(&a.token),
        Some(json!({ "ids": too_many })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_limit_is_honoured_and_bounded() {
    let h = pond().await;
    let a = paired(&h, "phone-a").await;
    for i in 0..3 {
        h.sender.broadcast(notice(&format!("n{i}"))).await.unwrap();
    }
    let (status, body) = call(
        &h.lan,
        Method::GET,
        "/api/v1/notifications/history?limit=2",
        Some(&a.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed: Vec<&str> = body["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(listed, vec!["n2", "n1"], "newest first, two of them");

    let (status, body) = call(
        &h.lan,
        Method::GET,
        "/api/v1/notifications/history?limit=0",
        Some(&a.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["notifications"].as_array().unwrap().len(),
        1,
        "a zero limit is raised to one, not an empty page"
    );
}
