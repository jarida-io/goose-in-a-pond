//! `/me/screenshots` over HTTP, as a paired phone of a household of one calls it: off until
//! the member turns it on, a bounded queue of pictures held in memory, and notes they can
//! read and delete.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use pond_api::build_router;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::domain::model_capabilities::ModelCapabilities;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_core::user_data::ports::screenshot_notes::ScreenshotNoteRepository;
use pond_core::user_data::services::screenshot_notes::{
    pending, ScreenshotNote, MAX_IMAGE_CHARS, MAX_PENDING,
};
use pond_infra::sqlite_screenshot_notes::SqliteScreenshotNoteRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

/// A model that says whether it reads pictures and is never asked anything here.
struct Model {
    vision: bool,
}

#[async_trait]
impl LlmProvider for Model {
    async fn complete(
        &self,
        _system: &str,
        _messages: Vec<ChatMessage>,
    ) -> anyhow::Result<ChatMessage> {
        Ok(ChatMessage::assistant("{}"))
    }

    fn model_name(&self) -> String {
        "test".to_string()
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            vision: self.vision,
            ..ModelCapabilities::default()
        }
    }
}

struct Pond {
    router: axum::Router,
    notes: SqliteScreenshotNoteRepository,
    member: String,
    _dir: tempfile::TempDir,
}

async fn a_pond(vision: bool) -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let member = state
        .profile_repo
        .create(CreateProfileRequest {
            display_name: "Jerry".into(),
            avatar_emoji: "x".into(),
        })
        .await
        .unwrap()
        .id;
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
    *state.llm_provider.write().await = Some(Arc::new(Model { vision }));
    let notes = SqliteScreenshotNoteRepository::new(state.db.system.clone());
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        notes,
        member,
        _dir: built.dir,
    }
}

async fn call(pond: &Pond, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/api/v1{uri}"))
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let peer: SocketAddr = "127.0.0.1:50000".parse().unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    let response = pond.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn picture() -> Value {
    json!({"image": "AAAA", "mime_type": "image/jpeg"})
}

#[tokio::test]
async fn screenshots_are_off_until_the_member_turns_them_on() {
    let pond = a_pond(true).await;
    let (status, hub) = call(&pond, Method::GET, "/me/hub", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hub["screenshots"], true);

    let (status, at_first) = call(&pond, Method::GET, "/me/screenshots", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{at_first}");
    assert_eq!(at_first["enabled"], false);
    assert_eq!(at_first["available"], true);
    assert_eq!(at_first["notes"], json!([]));

    let (status, refused) = call(&pond, Method::POST, "/me/screenshots", picture()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(
        pending().count_for(&pond.member),
        0,
        "nothing is held while off"
    );

    let (status, on) = call(
        &pond,
        Method::PUT,
        "/me/screenshots",
        json!({"enabled": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{on}");
    assert_eq!(on["enabled"], true);

    let (status, taken) = call(&pond, Method::POST, "/me/screenshots", picture()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{taken}");
    assert_eq!(taken["pending"], 1);

    let (status, wrong) = call(
        &pond,
        Method::POST,
        "/me/screenshots",
        json!({"image": "AAAA", "mime_type": "image/gif"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{wrong}");
    let (status, huge) = call(
        &pond,
        Method::POST,
        "/me/screenshots",
        json!({"image": "A".repeat(MAX_IMAGE_CHARS + 1), "mime_type": "image/png"}),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{huge}");

    for _ in 1..MAX_PENDING {
        let (status, _) = call(&pond, Method::POST, "/me/screenshots", picture()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
    let (status, full) = call(&pond, Method::POST, "/me/screenshots", picture()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{full}");

    // Turning it off forgets what was still waiting to be read.
    let (status, off) = call(
        &pond,
        Method::PUT,
        "/me/screenshots",
        json!({"enabled": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{off}");
    assert_eq!(off["enabled"], false);
    assert_eq!(off["pending"], 0);
    assert_eq!(pending().count_for(&pond.member), 0);
}

#[tokio::test]
async fn a_pond_whose_model_cannot_read_pictures_takes_none() {
    let pond = a_pond(false).await;
    call(
        &pond,
        Method::PUT,
        "/me/screenshots",
        json!({"enabled": true}),
    )
    .await;
    let (_, view) = call(&pond, Method::GET, "/me/screenshots", json!({})).await;
    assert_eq!(view["enabled"], true);
    assert_eq!(view["available"], false);
    let (status, refused) = call(&pond, Method::POST, "/me/screenshots", picture()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(pending().count_for(&pond.member), 0);
}

#[tokio::test]
async fn the_member_reads_their_notes_and_deletes_one_or_all() {
    let pond = a_pond(true).await;
    for (id, text) in [
        ("n1", "Looking at the Ngong Hills hiking trail"),
        ("n2", "Saved a recipe for chicken biryani"),
    ] {
        pond.notes
            .add(&ScreenshotNote {
                id: id.into(),
                profile_id: pond.member.clone(),
                note: text.into(),
                taken_at: None,
                created_at: Utc::now(),
                used_at: None,
            })
            .await
            .unwrap();
    }
    let (_, view) = call(&pond, Method::GET, "/me/screenshots", json!({})).await;
    let notes = view["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 2, "{view}");
    assert!(notes[0].get("profile_id").is_none());

    let (status, _) = call(&pond, Method::DELETE, "/me/screenshots/n1", json!({})).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&pond, Method::DELETE, "/me/screenshots/n1", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, gone) = call(&pond, Method::DELETE, "/me/screenshots", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{gone}");
    assert_eq!(gone["deleted"], 1);
    let (_, view) = call(&pond, Method::GET, "/me/screenshots", json!({})).await;
    assert_eq!(view["notes"], json!([]));
}
