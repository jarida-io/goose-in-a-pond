//! `/me/goose/activity` over HTTP, as a paired phone of a household of one calls it: what Goose
//! did by itself, how many entries are new, and taking one back.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use chrono::{Duration, Utc};
use pond_api::build_router;
use pond_core::shared::domain::agent_note::{AgentNote, NoteKind};
use pond_core::shared::domain::goose_activity::GooseActivity;
use pond_core::shared::ports::agent_notes::AgentNotesRepository;
use pond_core::shared::ports::goose_activity::GooseActivityRepository;
use pond_core::user_data::domain::profile::{CreateProfileRequest, ProfileScope};
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_infra::sqlite_agent_notes::SqliteAgentNotes;
use pond_infra::sqlite_goose_activity::SqliteGooseActivity;
use serde_json::{json, Value};
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

struct Pond {
    router: axum::Router,
    activity: SqliteGooseActivity,
    notes: SqliteAgentNotes,
    member: String,
    _dir: tempfile::TempDir,
}

async fn a_pond() -> Pond {
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
    let pool = state.db.system.clone();
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        activity: SqliteGooseActivity::new(pool.clone()),
        notes: SqliteAgentNotes::new(pool),
        member,
        _dir: built.dir,
    }
}

async fn call(pond: &Pond, method: Method, uri: &str) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/api/v1{uri}"))
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&json!({})).unwrap()))
        .unwrap();
    let peer: SocketAddr = "127.0.0.1:50000".parse().unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    let response = pond.router.clone().oneshot(request).await.unwrap();
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
async fn a_member_reads_what_goose_did_marks_it_seen_and_takes_a_note_back() {
    let pond = a_pond().await;
    let (_, hub) = call(&pond, Method::GET, "/me/hub").await;
    assert_eq!(hub["activity"], true);

    let now = Utc::now();
    let note = AgentNote {
        id: "n1".into(),
        profile_id: Some(pond.member.clone()),
        kind: NoteKind::Note,
        text: "Ask how the interview went".into(),
        created_at: now,
    };
    pond.notes.add(note.clone()).await.unwrap();
    let wrote = GooseActivity::note(&pond.member, &note.text, &note.id, now - Duration::minutes(5));
    let looked = GooseActivity::errand(&pond.member, "when the pool opens", "It opens at 6am.", now);
    pond.activity.add(&wrote).await.unwrap();
    pond.activity.add(&looked).await.unwrap();

    let (status, body) = call(&pond, Method::GET, "/me/goose/activity").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["unseen"], 2);
    let entries = body["activity"].as_array().unwrap();
    assert_eq!(entries[0]["kind"], "errand");
    assert_eq!(entries[0]["summary"], "Looked into: when the pool opens");
    assert_eq!(entries[0]["detail"], "It opens at 6am.");
    assert_eq!(entries[0]["can_undo"], false);
    assert_eq!(entries[1]["can_undo"], true);

    let (status, _) = call(&pond, Method::POST, "/me/goose/activity/seen").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = call(&pond, Method::GET, "/me/goose/activity").await;
    assert_eq!(body["unseen"], 0);

    // What cannot be taken back says so; a note taken back is gone, once.
    let undo = |id: &str| format!("/me/goose/activity/{id}/undo");
    let (status, _) = call(&pond, Method::POST, &undo(&looked.id)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, body) = call(&pond, Method::POST, &undo(&wrote.id)).await;
    assert_eq!((status, &body["undone"]), (StatusCode::OK, &json!(true)));
    let scope = ProfileScope::Owner(pond.member.clone());
    assert!(pond.notes.list(&scope, NoteKind::Note).await.unwrap().is_empty());
    let (status, _) = call(&pond, Method::POST, &undo(&wrote.id)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(&pond, Method::POST, &undo("no-such-entry")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, body) = call(&pond, Method::GET, "/me/goose/activity").await;
    assert_eq!(body["activity"][1]["undone"], true);
}
