//! `/api/v1/voice/room` through the real router and auth middleware, over a scripted room.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use futures::StreamExt;
use pond_api::test_support::{pair_phone, TestState, TEST_SPKI_PIN};
use pond_core::models::domain::room_voice::RoomUnavailable;
use pond_core::models::mocks::mock_room_voice::MockRoomVoice;
use pond_core::security::ports::handshake::Handshake;
use pond_core::shared::domain::agent::{InterruptReason, WorkflowEvent, WorkflowState};
use serde_json::{json, Value};
use tower::ServiceExt;

struct Pond {
    router: axum::Router,
    token: String,
    settings: Arc<dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync>,
    _dir: tempfile::TempDir,
}

async fn pond(room: Option<Arc<MockRoomVoice>>) -> Pond {
    let TestState {
        mut state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    state.room_voice =
        room.map(|r| r as Arc<dyn pond_core::models::ports::room_voice::RoomVoiceControl>);
    let settings = state.settings_repo.clone();
    let lan_ip = pond_api::network::interfaces()
        .unwrap()
        .into_iter()
        .find(pond_api::network::is_lan_interface)
        .map(|i| i.ip())
        .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
    let router = pond_api::build_companion_router(Arc::new(state))
        .layer(MockConnectInfo(SocketAddr::new(lan_ip, 40_000)));
    let code = handshake.issue_pairing_code().await.unwrap().code;
    let (status, paired) = pair_phone(&router, &code, "room-phone", TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{paired}");
    Pond {
        router,
        token: paired["session_token"].as_str().unwrap().to_string(),
        settings,
        _dir: dir,
    }
}

fn request(method: Method, uri: &str, bearer: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    match body {
        Some(v) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    }
}

async fn call(
    router: &axum::Router,
    method: Method,
    uri: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(request(method, uri, bearer, body))
        .await
        .unwrap();
    let status = response.status();
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        axum::body::to_bytes(response.into_body(), 1 << 20),
    )
    .await
    .map(|b| b.unwrap_or_default())
    .unwrap_or_default();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/v1/voice/room"),
    ("PUT", "/api/v1/voice/room"),
    ("GET", "/api/v1/voice/room/events"),
    ("POST", "/api/v1/voice/room/interrupt"),
    ("POST", "/api/v1/voice/room/pause"),
    ("POST", "/api/v1/voice/room/resume"),
];

#[tokio::test]
async fn every_room_route_wants_a_token() {
    let p = pond(Some(Arc::new(MockRoomVoice::default()))).await;
    for (method, uri) in ROUTES {
        let (status, _) = call(
            &p.router,
            method.parse().unwrap(),
            uri,
            None,
            Some(json!({"enabled": true})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn the_status_is_the_rooms_own() {
    let p = pond(Some(Arc::new(MockRoomVoice::unavailable(
        RoomUnavailable::VoiceRunningElsewhere,
    ))))
    .await;
    let (status, body) = call(
        &p.router,
        Method::GET,
        "/api/v1/voice/room",
        Some(&p.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["available"], false);
    assert_eq!(body["unavailable_reason"], "voice_running_elsewhere");
    assert_eq!(body["state"], "off");
    assert_eq!(body["wake"]["phrase"], "hey goose");
    assert_eq!(body["wake"]["detector"], "whisper");
    assert_eq!(body["interrupts"], "wake_only");
    assert!(body["last_heard"].is_null() && body["session_id"].is_null());
}

#[tokio::test]
async fn switching_the_room_off_saves_the_choice_and_answers_with_the_status() {
    let room = Arc::new(MockRoomVoice::default());
    let p = pond(Some(room.clone())).await;
    let (status, body) = call(
        &p.router,
        Method::PUT,
        "/api/v1/voice/room",
        Some(&p.token),
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["state"], "off");
    assert_eq!(
        p.settings
            .get_key("voice_room_enabled")
            .await
            .unwrap()
            .as_deref(),
        Some("false")
    );

    let (status, _) = call(
        &p.router,
        Method::PUT,
        "/api/v1/voice/room",
        Some(&p.token),
        Some(json!({"on": "yes"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_interrupt_reaches_the_room() {
    let room = Arc::new(MockRoomVoice::default());
    let p = pond(Some(room.clone())).await;
    let (status, _) = call(
        &p.router,
        Method::POST,
        "/api/v1/voice/room/interrupt",
        Some(&p.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(room.interrupts(), 1);
}

/// While a member teaches the pond their voice, every sentence starts with the wake phrase.
#[tokio::test]
async fn a_pause_deafens_the_room_for_a_while_and_a_resume_ends_it() {
    let room = Arc::new(MockRoomVoice::default());
    let p = pond(Some(room.clone())).await;
    let (status, _) = call(
        &p.router,
        Method::POST,
        "/api/v1/voice/room/pause",
        Some(&p.token),
        Some(json!({"seconds": 120})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = call(
        &p.router,
        Method::GET,
        "/api/v1/voice/room",
        Some(&p.token),
        None,
    )
    .await;
    assert_eq!(body["state"], "paused", "{body}");
    let until = chrono::DateTime::parse_from_rfc3339(body["paused_until"].as_str().unwrap())
        .unwrap()
        .with_timezone(&chrono::Utc);
    let left = until - chrono::Utc::now();
    assert!(left > chrono::Duration::seconds(100) && left <= chrono::Duration::seconds(120));

    let (status, _) = call(
        &p.router,
        Method::POST,
        "/api/v1/voice/room/resume",
        Some(&p.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = call(
        &p.router,
        Method::GET,
        "/api/v1/voice/room",
        Some(&p.token),
        None,
    )
    .await;
    assert_eq!(body["state"], "wait");
    assert_eq!(body["paused_until"], Value::Null);
}

#[tokio::test]
async fn a_pause_is_one_to_three_hundred_seconds() {
    let p = pond(Some(Arc::new(MockRoomVoice::default()))).await;
    for body in [
        json!({"seconds": 0}),
        json!({"seconds": 301}),
        json!({}),
        json!({"seconds": "ten"}),
    ] {
        let (status, _) = call(
            &p.router,
            Method::POST,
            "/api/v1/voice/room/pause",
            Some(&p.token),
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    let (status, _) = call(
        &p.router,
        Method::POST,
        "/api/v1/voice/room/pause",
        Some(&p.token),
        Some(json!({"seconds": 300})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_pond_without_room_voice_says_so() {
    let p = pond(None).await;
    let (status, body) = call(
        &p.router,
        Method::GET,
        "/api/v1/voice/room",
        Some(&p.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["reason"], "room_voice_unavailable");
}

type Events =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Send>>;

/// An SSE body and what has been read of it but not yet taken.
struct Sse {
    stream: Events,
    buffer: String,
}

/// The next `data:` line, skipping keep-alive comments.
async fn next_data(sse: &mut Sse) -> String {
    loop {
        if let Some(at) = sse.buffer.find("\n\n") {
            let frame: String = sse.buffer.drain(..at + 2).collect();
            if let Some(data) = frame.lines().find_map(|l| l.strip_prefix("data: ")) {
                return data.to_string();
            }
            continue;
        }
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(2), sse.stream.next())
            .await
            .expect("an event in time")
            .expect("the stream stays open")
            .unwrap();
        sse.buffer.push_str(&String::from_utf8_lossy(&bytes));
    }
}

async fn subscribe(p: &Pond, room: &MockRoomVoice, uri: &str) -> Sse {
    let before = room.subscribers();
    let response = p
        .router
        .clone()
        .oneshot(request(Method::GET, uri, Some(&p.token), None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    let mut sse = Sse {
        stream: Box::pin(response.into_body().into_data_stream()),
        buffer: String::new(),
    };
    // The subscription is made when the stream is first polled.
    if let Ok(Some(Ok(bytes))) =
        tokio::time::timeout(std::time::Duration::from_millis(50), sse.stream.next()).await
    {
        sse.buffer.push_str(&String::from_utf8_lossy(&bytes));
    }
    assert_eq!(room.subscribers(), before + 1);
    sse
}

#[tokio::test]
async fn the_event_stream_carries_the_loops_ndjson_and_levels_only_when_asked() {
    let room = Arc::new(MockRoomVoice::default());
    let p = pond(Some(room.clone())).await;
    let mut plain = subscribe(&p, &room, "/api/v1/voice/room/events").await;
    let mut with_levels = subscribe(&p, &room, "/api/v1/voice/room/events?levels=1").await;

    room.send(WorkflowEvent::AudioLevel { rms: 0.25 });
    room.send(WorkflowEvent::Wake { who: None });
    room.send(WorkflowEvent::StateChanged {
        state: WorkflowState::Listen,
    });
    room.send(WorkflowEvent::Interrupt {
        why: InterruptReason::WakeWord,
    });

    assert_eq!(
        next_data(&mut plain).await,
        r#"{"event":"wake","who":null}"#
    );
    assert_eq!(
        next_data(&mut plain).await,
        r#"{"event":"state","state":"listen"}"#
    );
    assert_eq!(
        next_data(&mut plain).await,
        r#"{"event":"interrupt","why":"wake_word"}"#
    );
    assert_eq!(
        next_data(&mut with_levels).await,
        r#"{"event":"audio_level","rms":0.25}"#
    );
    assert_eq!(
        next_data(&mut with_levels).await,
        r#"{"event":"wake","who":null}"#
    );
}
