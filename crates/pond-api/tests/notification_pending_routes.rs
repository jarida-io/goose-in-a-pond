//! A household notice reaches every paired phone durably: each phone gets its own queued copy,
//! fetches it with `GET /notifications/pending` after a push wake, and acknowledges it with
//! `POST /notifications/{id}/ack`. Both answer for the caller's own device only, named by its
//! token. A phone with its stream open sees the notice once, under its own copy's id.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use futures::StreamExt;
use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
use pond_api::{build_router, AppState};
use pond_core::mcp::ports::notification::{
    per_device_notification_id, Notification, NotificationSender,
};
use pond_core::security::ports::handshake::Handshake;
use pond_infra::broadcast_notification_sender::BroadcastNotificationSender;
use pond_infra::sqlite_notification_queue::SqliteNotificationQueue;
use pond_infra::sqlite_notification_recipients::SqliteNotificationRecipients;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Harness {
    loopback: axum::Router,
    lan: axum::Router,
    handshake: Arc<pond_infra::sqlite_handshake::SqliteHandshakeAdapter>,
    sender: Arc<dyn NotificationSender>,
    pool: sqlx::SqlitePool,
    _dir: tempfile::TempDir,
}

/// A pond wired as `main.rs` wires it: the real queue, the real recipients, one channel.
async fn pond() -> Harness {
    let pond_api::test_support::TestState {
        mut state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    let pool = state.db.system.clone();
    let queue = Arc::new(SqliteNotificationQueue::new(pool.clone()));
    let (tx, _) = tokio::sync::broadcast::channel::<Notification>(64);
    let sender: Arc<dyn NotificationSender> = Arc::new(
        BroadcastNotificationSender::new(tx.clone(), queue.clone(), None)
            .with_household_recipients(Arc::new(SqliteNotificationRecipients::new(pool.clone()))),
    );
    state.notification_tx = tx;
    state.notification_queue = Some(queue);
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
        pool,
        _dir: dir,
    }
}

/// A paired phone: its session token and the device id the pond gave it.
struct Phone {
    token: String,
    device: String,
}

async fn paired(h: &Harness, client_id: &str) -> Phone {
    let code = h.handshake.issue_pairing_code().await.unwrap().code;
    let (status, body) = pair_phone(&h.loopback, &code, client_id, TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["session_token"].as_str().unwrap().to_string();
    let (status, me) = send(&h.lan, Method::GET, "/api/v1/devices/self", Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    Phone {
        token,
        device: me["device_id"].as_str().unwrap().to_string(),
    }
}

async fn respond(
    router: &axum::Router,
    method: Method,
    path: &str,
    bearer: Option<&str>,
) -> axum::response::Response {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn send(
    router: &axum::Router,
    method: Method,
    path: &str,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let response = respond(router, method, path, bearer).await;
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
        category: "info".into(),
        title: "Schedule: water the plants".into(),
        body: "Done".into(),
        timestamp: "2026-10-20T07:00:00Z".into(),
        data: None,
    }
}

fn ack_path(id: &str) -> String {
    // The per-device id holds a `:`; a client percent-encodes the segment as GOTG does.
    format!("/api/v1/notifications/{}/ack", id.replace(':', "%3A"))
}

/// Pairing is itself a household notice, so each phone starts with the pairing notices queued
/// for it; acknowledge them so a test sees only its own.
async fn settle(h: &Harness, phones: &[&Phone]) {
    for phone in phones {
        for id in pending_ids(h, phone).await {
            let (status, body) =
                send(&h.lan, Method::POST, &ack_path(&id), Some(&phone.token)).await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
        assert!(pending_ids(h, phone).await.is_empty());
    }
}

async fn pending_ids(h: &Harness, phone: &Phone) -> Vec<String> {
    let (status, body) = send(
        &h.lan,
        Method::GET,
        "/api/v1/notifications/pending",
        Some(&phone.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["notifications"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn each_phone_fetches_and_acknowledges_only_its_own_copy() {
    let h = pond().await;
    let a = paired(&h, "phone-a").await;
    let b = paired(&h, "phone-b").await;
    assert_eq!(
        pending_ids(&h, &a).await.len(),
        2,
        "phone A was asleep for both pairings, its own and B's, and has both notices waiting"
    );
    settle(&h, &[&a, &b]).await;

    h.sender.broadcast(notice("sched-1")).await.unwrap();

    let a_copy = per_device_notification_id("sched-1", &a.device);
    let b_copy = per_device_notification_id("sched-1", &b.device);

    let (status, body) = send(
        &h.lan,
        Method::GET,
        "/api/v1/notifications/pending",
        Some(&a.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"notifications": [{
            "id": a_copy,
            "target": a.device,
            "category": "info",
            "title": "Schedule: water the plants",
            "body": "Done",
            "timestamp": "2026-10-20T07:00:00Z",
            "data": null,
        }]}),
        "a phone's pending list is its own copy, in the stream's shape"
    );
    assert_eq!(pending_ids(&h, &b).await, vec![b_copy.clone()]);

    // Cross-device: A names B's copy. Refused as if it did not exist, and B's copy still waits.
    let (status, body) = send(&h.lan, Method::POST, &ack_path(&b_copy), Some(&a.token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(pending_ids(&h, &b).await, vec![b_copy.clone()]);

    let (status, body) = send(&h.lan, Method::POST, &ack_path(&a_copy), Some(&a.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"id": a_copy, "acknowledged": true}));
    assert!(pending_ids(&h, &a).await.is_empty());
    assert_eq!(
        pending_ids(&h, &b).await,
        vec![b_copy],
        "acknowledging one phone's copy leaves the other's"
    );

    let (status, _) = send(&h.lan, Method::POST, &ack_path(&a_copy), Some(&a.token)).await;
    assert_eq!(status, StatusCode::OK, "acknowledging twice is harmless");
}

#[tokio::test]
async fn pending_and_ack_need_a_paired_device_token() {
    let h = pond().await;
    let phone = paired(&h, "phone-a").await;
    h.sender.broadcast(notice("secret-plan")).await.unwrap();
    let own = per_device_notification_id("secret-plan", &phone.device);
    assert!(
        pending_ids(&h, &phone).await.contains(&own),
        "vacuity control: there is a queued notice a refused caller could leak"
    );

    // The same token, revoked: what an unpaired phone still holds.
    let revoked = paired(&h, "phone-revoked").await;
    h.handshake.revoke_token(&revoked.token).await.unwrap();

    for (who, bearer) in [
        ("no token", None),
        ("a malformed token", Some("not-a-token")),
        ("a revoked token", Some(revoked.token.as_str())),
    ] {
        for (method, path) in [
            (Method::GET, "/api/v1/notifications/pending".to_string()),
            (Method::POST, ack_path(&own)),
        ] {
            let response = respond(&h.lan, method.clone(), &path, bearer).await;
            // Status first: a body predicate alone would pass against an error payload.
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path} with {who}"
            );
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains("secret-plan")
                    && !text.contains("water the plants")
                    && !text.contains("notifications")
                    && !text.contains("acknowledged"),
                "{method} {path} with {who} leaked: {text}"
            );
        }
    }

    assert!(
        pending_ids(&h, &phone).await.contains(&own),
        "a refused ack changed nothing"
    );
}

type SseBody =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Send>>;

/// Open the phone's stream without polling it yet.
async fn open_stream(h: &Harness, phone: &Phone) -> SseBody {
    let stream = format!("/api/v1/notifications/stream?device_id={}", phone.device);
    let response = respond(&h.lan, Method::GET, &stream, Some(&phone.token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    Box::pin(response.into_body().into_data_stream())
}

/// The ids of every notification the stream sends until it goes quiet; keep-alives carry no
/// `data:` line. Draining also runs the stream's post-yield `mark_delivered`.
async fn ids_until_quiet(body: &mut SseBody) -> Vec<String> {
    let mut ids = Vec::new();
    while let Ok(Some(Ok(chunk))) =
        tokio::time::timeout(std::time::Duration::from_millis(300), body.next()).await
    {
        for line in String::from_utf8_lossy(&chunk).lines() {
            if let Some(data) = line.strip_prefix("data:") {
                let n: Value = serde_json::from_str(data.trim()).unwrap();
                ids.push(n["id"].as_str().unwrap().to_string());
            }
        }
    }
    ids
}

/// The live broadcast copy and the phone's queued copy are one notice. Streaming both would show
/// it twice, since the app dedupes by id and the two ids differ.
#[tokio::test]
async fn a_connected_phone_sees_a_household_notice_once() {
    let h = pond().await;
    let a = paired(&h, "phone-a").await;
    settle(&h, &[&a]).await;

    let mut body = open_stream(&h, &a).await;
    assert!(
        ids_until_quiet(&mut body).await.is_empty(),
        "nothing waiting"
    );

    h.sender.broadcast(notice("sched-2")).await.unwrap();

    assert_eq!(
        ids_until_quiet(&mut body).await,
        vec![per_device_notification_id("sched-2", &a.device)],
        "the phone gets its own copy live and not the broadcast copy as well"
    );
    drop(body);
    assert_eq!(
        pending_ids(&h, &a).await,
        vec![per_device_notification_id("sched-2", &a.device)],
        "streaming the copy is no proof it arrived; it waits for the phone's ack"
    );
}

/// A notice queued while the stream is connecting is both in the drain and on the live channel
/// the stream already subscribed to; it is still sent once.
#[tokio::test]
async fn a_notice_that_lands_while_the_stream_connects_is_sent_once() {
    let h = pond().await;
    let a = paired(&h, "phone-a").await;
    settle(&h, &[&a]).await;

    // Subscribed, not yet drained: the drain runs on the first poll.
    let mut body = open_stream(&h, &a).await;
    h.sender.broadcast(notice("sched-3")).await.unwrap();

    assert_eq!(
        ids_until_quiet(&mut body).await,
        vec![per_device_notification_id("sched-3", &a.device)]
    );
}

/// When the copy was first stamped delivered, read straight from the table.
async fn delivered_at(h: &Harness, id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT delivered_at FROM notifications WHERE id = ?")
        .bind(id)
        .fetch_one(&h.pool)
        .await
        .unwrap()
}

/// The Jetson report of 2026-10-20: the phone dropped off the network with its stream still
/// held open, a Run now wrote the notice into the dead connection, and the stream marked it
/// delivered, so the wake's `/pending` came back empty. A write is no proof of arrival.
#[tokio::test]
async fn a_stream_write_leaves_a_phones_copy_undelivered_until_it_is_acked() {
    let h = pond().await;
    let a = paired(&h, "phone-a").await;
    settle(&h, &[&a]).await;
    let copy = per_device_notification_id("run-now", &a.device);

    // Connected before the notice and on the drain path after it: both writes count.
    let mut body = open_stream(&h, &a).await;
    assert!(ids_until_quiet(&mut body).await.is_empty());
    h.sender.broadcast(notice("run-now")).await.unwrap();
    assert_eq!(ids_until_quiet(&mut body).await, vec![copy.clone()]);
    drop(body);
    let mut body = open_stream(&h, &a).await;
    assert_eq!(ids_until_quiet(&mut body).await, vec![copy.clone()]);
    drop(body);

    assert_eq!(
        delivered_at(&h, &copy).await,
        None,
        "neither the live write nor the connect drain stamps the copy"
    );
    assert_eq!(
        pending_ids(&h, &a).await,
        vec![copy.clone()],
        "so the push wake still finds it"
    );

    let (status, body) = send(&h.lan, Method::POST, &ack_path(&copy), Some(&a.token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        delivered_at(&h, &copy).await.is_some(),
        "the ack is what stamps it"
    );
    assert!(pending_ids(&h, &a).await.is_empty());
}

/// Every new connection re-sends what the phone has not acknowledged, and only that.
#[tokio::test]
async fn a_reconnecting_phone_is_resent_every_unacked_copy() {
    let h = pond().await;
    let a = paired(&h, "phone-a").await;
    settle(&h, &[&a]).await;
    h.sender.broadcast(notice("first")).await.unwrap();
    h.sender.broadcast(notice("second")).await.unwrap();
    let first = per_device_notification_id("first", &a.device);
    let second = per_device_notification_id("second", &a.device);

    let mut body = open_stream(&h, &a).await;
    assert_eq!(
        ids_until_quiet(&mut body).await,
        vec![first.clone(), second.clone()]
    );
    drop(body);

    let (status, _) = send(&h.lan, Method::POST, &ack_path(&first), Some(&a.token)).await;
    assert_eq!(status, StatusCode::OK);

    let mut body = open_stream(&h, &a).await;
    assert_eq!(
        ids_until_quiet(&mut body).await,
        vec![second.clone()],
        "the acked copy is not sent again; the unacked one is"
    );
    drop(body);
    assert_eq!(pending_ids(&h, &a).await, vec![second]);
}

/// A desktop or any other non-phone listener has no ack path; its stream marks what it sends.
#[tokio::test]
async fn a_non_phone_stream_still_marks_what_it_sends_delivered() {
    let h = pond().await;
    let desk = paired(&h, "desk").await;
    settle(&h, &[&desk]).await;
    sqlx::query("UPDATE devices SET device_type = 'desktop' WHERE id = ?")
        .bind(&desk.device)
        .execute(&h.pool)
        .await
        .unwrap();

    let mut targeted = notice("for-desk");
    targeted.target = desk.device.clone();
    h.sender.send(targeted).await.unwrap();
    assert_eq!(
        pending_ids(&h, &desk).await,
        vec!["for-desk".to_string()],
        "vacuity control: the row is queued before the stream sends it"
    );

    let mut body = open_stream(&h, &desk).await;
    assert_eq!(
        ids_until_quiet(&mut body).await,
        vec!["for-desk".to_string()]
    );
    drop(body);
    assert!(delivered_at(&h, "for-desk").await.is_some());
    assert!(pending_ids(&h, &desk).await.is_empty());
}
