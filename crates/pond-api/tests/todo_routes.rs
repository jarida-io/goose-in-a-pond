//! My Goose's to-dos over HTTP: each member reaches only their own to-dos, places and offers,
//! a malformed body is refused by field, and an offered mention becomes a to-do once.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, TokenCaller,
};
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::domain::reminder::{CapturedReminder, ReminderDisposition};
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_core::user_data::ports::reminder_repository::ReminderRepository;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use pond_infra::sqlite_reminder::SqliteReminderRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

/// `jerry-token` is Jerry's phone and `emmanuel-token` Emmanuel's.
struct TwoPhones {
    jerry: String,
    emmanuel: String,
}

#[async_trait]
impl Handshake for TwoPhones {
    async fn revoke_device(&self, _device_id: &str) -> Result<u64> {
        Ok(0)
    }
    async fn handshake(&self, _: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("pairing is not used by this fixture")
    }
    async fn validate_token(&self, token: &str) -> Result<bool> {
        Ok(matches!(token, "jerry-token" | "emmanuel-token"))
    }
    async fn caller_for_token(&self, token: &str) -> Result<Option<TokenCaller>> {
        let device = match token {
            "jerry-token" => &self.jerry,
            "emmanuel-token" => &self.emmanuel,
            _ => return Ok(None),
        };
        Ok(Some(TokenCaller {
            client_id: device.clone(),
            device_id: device.clone(),
        }))
    }
    async fn revoke_token(&self, _: &str) -> Result<()> {
        anyhow::bail!("revocation is not used by this fixture")
    }
}

struct Pond {
    router: axum::Router,
    jerry: String,
    reminders: SqliteReminderRepository,
    _dir: tempfile::TempDir,
}

async fn pond() -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let pool = state.db.system.clone();
    state.skip_onboarding = true;
    let mut members = Vec::new();
    for name in ["Jerry", "Emmanuel"] {
        let member = state
            .profile_repo
            .create(CreateProfileRequest {
                display_name: name.into(),
                avatar_emoji: "duck".into(),
            })
            .await
            .unwrap()
            .id;
        let device = state
            .device_registry
            .register(RegisterDeviceRequest {
                id: None,
                name: format!("{name}'s phone"),
                device_type: "gotg".into(),
                hostname: None,
                capabilities: vec![],
                room_id: None,
            })
            .await
            .unwrap()
            .id;
        SqliteDeviceAttribution::new(pool.clone())
            .set_device_profile(&device, Some(&member))
            .await
            .unwrap();
        members.push((member, device));
    }
    state.handshake = Arc::new(TwoPhones {
        jerry: members[0].1.clone(),
        emmanuel: members[1].1.clone(),
    });
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        jerry: members[0].0.clone(),
        reminders: SqliteReminderRepository::new(pool),
        _dir: built.dir,
    }
}

async fn call(
    pond: &Pond,
    token: &str,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("/api/v1{uri}"))
        .header("Authorization", format!("Bearer {token}"));
    let body = match body {
        Some(value) => {
            builder = builder.header("Content-Type", "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    let response = pond
        .router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn the_hub_says_todos_are_served() {
    let p = pond().await;
    let (status, body) = call(&p, "jerry-token", Method::GET, "/me/hub", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["todos"], true);
}

#[tokio::test]
async fn a_member_reaches_only_their_own_todos() {
    let p = pond().await;
    let (status, todo) = call(
        &p,
        "jerry-token",
        Method::POST,
        "/me/todos",
        Some(json!({
            "kind": "todo",
            "text": "call the garage about the Fielder",
            "hooks": [{"kind": "repeat", "spec": {"every": "weekday", "at": "18:00"}}],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{todo}");
    assert_eq!(todo["hooks"][0]["label"], "Every weekday at 18:00");
    assert_eq!(todo["ask"], true);
    assert_eq!(todo["source"], "you");
    assert!(todo["hooks"][0]["next_fire_at"].is_string());
    let id = todo["id"].as_str().unwrap().to_string();

    let (_, mine) = call(&p, "jerry-token", Method::GET, "/me/todos", None).await;
    assert_eq!(mine["todos"].as_array().unwrap().len(), 1);
    let (_, theirs) = call(
        &p,
        "emmanuel-token",
        Method::GET,
        "/me/todos?state=all",
        None,
    )
    .await;
    assert_eq!(theirs["todos"], json!([]));

    let path = format!("/me/todos/{id}");
    for (method, uri, body) in [
        (
            Method::PATCH,
            path.clone(),
            Some(json!({"text": "mine now"})),
        ),
        (Method::DELETE, path.clone(), None),
        (
            Method::POST,
            format!("{path}/answer"),
            Some(json!({"answer": "done"})),
        ),
    ] {
        let (status, _) = call(&p, "emmanuel-token", method.clone(), &uri, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
    }

    let (status, edited) = call(
        &p,
        "jerry-token",
        Method::PATCH,
        &path,
        Some(json!({"notes": "the Fielder's brakes", "hooks": []})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["hooks"], json!([]));
    assert_eq!(edited["text"], "call the garage about the Fielder");

    let (status, answered) = call(
        &p,
        "jerry-token",
        Method::POST,
        &format!("{path}/answer"),
        Some(json!({"answer": "not_yet", "snooze_minutes": 30})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answered}");
    assert!(answered["snoozed_until"].is_string());
    let (_, done) = call(
        &p,
        "jerry-token",
        Method::POST,
        &format!("{path}/answer"),
        Some(json!({"answer": "done"})),
    )
    .await;
    assert_eq!(
        (done["state"].clone(), done["snoozed_until"].clone()),
        (json!("done"), Value::Null)
    );
    assert!(done["done_at"].is_string());
    let (status, _) = call(
        &p,
        "jerry-token",
        Method::POST,
        &format!("{path}/answer"),
        Some(json!({"answer": "not_yet"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "never asked again once done");
    let (_, open) = call(&p, "jerry-token", Method::GET, "/me/todos", None).await;
    assert_eq!(open["todos"], json!([]));
    let (_, finished) = call(&p, "jerry-token", Method::GET, "/me/todos?state=done", None).await;
    assert_eq!(finished["todos"].as_array().unwrap().len(), 1);

    let (status, _) = call(&p, "jerry-token", Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_bad_body_is_refused_by_field() {
    let p = pond().await;
    for (body, says) in [
        (json!({"kind": "todo"}), "`text` is required"),
        (json!({"text": "x", "due_at": "Friday"}), "RFC 3339"),
        (
            json!({"text": "x", "hooks": [{"kind": "place", "spec": {"place_id": "nowhere"}}]}),
            "no such place",
        ),
        (
            json!({"text": "x", "hooks": [{"kind": "event", "spec": {"source_id": "cart:not-mine", "event": "price_drop"}}]}),
            "no such source",
        ),
        (
            json!({"text": "x", "hooks": [{"kind": "connector", "spec": {"connector_id": "webhook:none", "event": "call"}}]}),
            "no such connector",
        ),
    ] {
        let (status, refused) = call(
            &p,
            "jerry-token",
            Method::POST,
            "/me/todos",
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            refused["error"].as_str().unwrap_or_default().contains(says),
            "{body}: {refused}"
        );
    }
    let (status, _) = call(
        &p,
        "jerry-token",
        Method::GET,
        "/me/todos?state=someday",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &p,
        "jerry-token",
        Method::POST,
        "/me/places",
        Some(json!({"label": "Home", "lat": 120.0, "lng": 36.8})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn places_are_hook_sources_and_the_phones_geofence_is_queued() {
    let p = pond().await;
    let (status, place) = call(
        &p,
        "jerry-token",
        Method::POST,
        "/me/places",
        Some(json!({"label": "Home", "lat": -1.29, "lng": 36.82})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{place}");
    assert_eq!(place["radius_m"], 150.0);
    let place_id = place["id"].as_str().unwrap().to_string();

    let (_, sources) = call(&p, "jerry-token", Method::GET, "/me/hooks/sources", None).await;
    assert_eq!(sources["places"][0]["id"], json!(place_id));
    let presence = &sources["sources"][0];
    assert_eq!(presence["id"], "presence:home");
    assert_eq!(presence["kind"], "presence");
    assert_eq!(
        presence["events"][0],
        json!({"id": "arrive", "label": "You get home"})
    );
    let (_, theirs) = call(&p, "emmanuel-token", Method::GET, "/me/hooks/sources", None).await;
    assert_eq!(theirs["places"], json!([]));

    let (status, todo) = call(
        &p,
        "jerry-token",
        Method::POST,
        "/me/todos",
        Some(json!({
            "kind": "reminder",
            "text": "call Emmanuel",
            "hooks": [{"kind": "place", "spec": {"place_id": place_id, "transition": "arrive"}}],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{todo}");
    assert_eq!(todo["hooks"][0]["label"], "When you get home");
    assert_eq!(todo["ask"], false);

    let event = format!("/me/places/{place_id}/event");
    let (status, _) = call(
        &p,
        "jerry-token",
        Method::POST,
        &event,
        Some(json!({"transition": "arrive"})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, _) = call(
        &p,
        "jerry-token",
        Method::POST,
        &event,
        Some(json!({"transition": "hover"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &p,
        "emmanuel-token",
        Method::POST,
        &event,
        Some(json!({"transition": "arrive"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not their place");

    let (status, _) = call(
        &p,
        "jerry-token",
        Method::DELETE,
        &format!("/me/places/{place_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, after) = call(&p, "jerry-token", Method::GET, "/me/todos", None).await;
    assert_eq!(
        after["todos"][0]["hooks"],
        json!([]),
        "a deleted place takes its hooks"
    );
}

fn mention(id: &str, owner: &str, when_said: &str) -> CapturedReminder {
    let said = chrono::Utc::now();
    CapturedReminder {
        id: id.into(),
        about: "call the garage".into(),
        when_said: when_said.into(),
        session_id: "s1".into(),
        window_id: format!("w-{id}"),
        subject: "Jerry".into(),
        profile_id: Some(owner.into()),
        said_at: said,
        captured_at: said,
        disposition: ReminderDisposition::Pending,
    }
}

#[tokio::test]
async fn an_offered_mention_becomes_a_reminder_once_or_is_declined() {
    let p = pond().await;
    let now = chrono::Utc::now();
    for (id, when) in [
        ("r-tomorrow", "tomorrow at 6pm"),
        ("r-vague", "after the rains"),
        ("r-no", "on Friday"),
    ] {
        p.reminders
            .capture(&mention(id, &p.jerry, when))
            .await
            .unwrap();
        assert!(p.reminders.offer(id, now).await.unwrap());
    }
    let (_, offers) = call(&p, "jerry-token", Method::GET, "/me/todos/offers", None).await;
    let offers = offers["offers"].as_array().unwrap().clone();
    assert_eq!(offers.len(), 3);
    let by_id = |id: &str| {
        offers
            .iter()
            .find(|o| o["reminder_id"] == id)
            .unwrap()
            .clone()
    };
    assert!(by_id("r-tomorrow")["due_at"].is_string());
    assert_eq!(by_id("r-vague")["due_at"], Value::Null);
    let (_, none) = call(&p, "emmanuel-token", Method::GET, "/me/todos/offers", None).await;
    assert_eq!(none["offers"], json!([]));

    let (status, refused) = call(
        &p,
        "jerry-token",
        Method::POST,
        "/me/todos/offers/r-vague/accept",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(refused["error"]
        .as_str()
        .unwrap()
        .starts_with("due_at_required"));

    let (status, todo) = call(
        &p,
        "jerry-token",
        Method::POST,
        "/me/todos/offers/r-tomorrow/accept",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{todo}");
    assert_eq!(todo["kind"], "reminder");
    assert_eq!(todo["source"], "chat");
    assert_eq!(todo["hooks"][0]["kind"], "time");
    assert_eq!(todo["due_at"], by_id("r-tomorrow")["due_at"]);
    let (status, _) = call(
        &p,
        "jerry-token",
        Method::POST,
        "/me/todos/offers/r-tomorrow/accept",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "accepted once");

    let later = (now + chrono::Duration::days(2)).to_rfc3339();
    let (status, todo) = call(
        &p,
        "jerry-token",
        Method::POST,
        "/me/todos/offers/r-vague/accept",
        Some(json!({"due_at": later})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{todo}");

    let (status, _) = call(
        &p,
        "emmanuel-token",
        Method::POST,
        "/me/todos/offers/r-no/decline",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not their offer");
    let (status, _) = call(
        &p,
        "jerry-token",
        Method::POST,
        "/me/todos/offers/r-no/decline",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, left) = call(&p, "jerry-token", Method::GET, "/me/todos/offers", None).await;
    assert_eq!(left["offers"], json!([]));
    let (_, todos) = call(&p, "jerry-token", Method::GET, "/me/todos", None).await;
    assert_eq!(todos["todos"].as_array().unwrap().len(), 2);
}
