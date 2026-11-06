//! PAI-1 over HTTP: a member's phone reaches its own memories and conversations and the
//! household's, never another member's, and a device paired to nobody saves no memory.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::user_data::domain::memory::MemoryFragment;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::domain::session::{IdentificationSource, SessionIdentity};
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
#[allow(unused_imports)]
use pond_core::user_data::ports::profile::ProfileRepository as _;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use pond_infra::sqlite_memory::SqliteMemoryRepository;
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

struct Pond {
    router: axum::Router,
    memories: Arc<SqliteMemoryRepository>,
    sessions: Arc<dyn SessionStorage>,
    amina: String,
    baraka: String,
    _dir: tempfile::TempDir,
}

/// Two members, Amina and Baraka; every request comes from one phone, Amina's when `amina_phone`,
/// else nobody's.
async fn pond(amina_phone: bool) -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let memories = Arc::new(SqliteMemoryRepository::new(state.db.system.clone()));
    state.memory_repo = memories.clone();
    let mut members = Vec::new();
    for name in ["Amina", "Baraka"] {
        members.push(
            state
                .profile_repo
                .create(CreateProfileRequest {
                    display_name: name.into(),
                    avatar_emoji: "duck".into(),
                })
                .await
                .unwrap()
                .id,
        );
    }
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
    if amina_phone {
        SqliteDeviceAttribution::new(state.db.system.clone())
            .set_device_profile(&device_id, Some(&members[0]))
            .await
            .unwrap();
    }
    state.handshake = Arc::new(DeviceHandshake(device_id));
    let sessions = state.session_storage.clone();
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        memories,
        sessions,
        amina: members[0].clone(),
        baraka: members[1].clone(),
        _dir: built.dir,
    }
}

fn call(method: Method, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", "Bearer test-token");
    let body = match body {
        Some(value) => {
            builder = builder.header("Content-Type", "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    builder.body(body).unwrap()
}

async fn send(pond: &Pond, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = pond.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, body)
}

async fn remember(pond: &Pond, id: &str, owner: Option<&str>) {
    let mut fragment = MemoryFragment::from_chat(id.to_string(), None, None, format!("fact {id}"));
    fragment.profile_id = owner.map(str::to_string);
    pond.memories.add(fragment).await.unwrap();
}

async fn conversation(pond: &Pond, id: &str, owner: Option<&str>) {
    pond.sessions.create_session(id.to_string()).await.unwrap();
    if let Some(owner) = owner {
        pond.sessions
            .set_session_identity(
                id,
                &SessionIdentity {
                    profile_id: Some(owner.to_string()),
                    source: IdentificationSource::PairedDevice,
                    confidence: None,
                },
            )
            .await
            .unwrap();
    }
}

fn ids(body: &serde_json::Value) -> Vec<String> {
    let mut ids: Vec<String> = body
        .as_array()
        .or_else(|| body["sessions"].as_array())
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn a_members_phone_lists_her_memories_and_the_households_only() {
    let pond = pond(true).await;
    let (amina, baraka) = (pond.amina.clone(), pond.baraka.clone());
    remember(&pond, "a1", Some(&amina)).await;
    remember(&pond, "b1", Some(&baraka)).await;
    remember(&pond, "h1", None).await;

    let (status, body) = send(&pond, call(Method::GET, "/api/v1/memories", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["a1", "h1"]);
}

#[tokio::test]
async fn another_members_memory_cannot_be_changed_or_deleted() {
    let pond = pond(true).await;
    let baraka = pond.baraka.clone();
    remember(&pond, "b1", Some(&baraka)).await;

    let (status, _) = send(
        &pond,
        call(
            Method::PUT,
            "/api/v1/memories/b1",
            Some(serde_json::json!({"content": "rewritten"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&pond, call(Method::DELETE, "/api/v1/memories/b1", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(pond
        .memories
        .get_in_scope(
            "b1",
            &pond_core::user_data::domain::profile::ProfileScope::Household
        )
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn a_saved_memory_is_the_callers_unless_she_shares_it() {
    let pond = pond(true).await;
    let (_, mine) = send(
        &pond,
        call(
            Method::POST,
            "/api/v1/memories",
            Some(serde_json::json!({"content": "Amina's own"})),
        ),
    )
    .await;
    let (_, shared) = send(
        &pond,
        call(
            Method::POST,
            "/api/v1/memories",
            Some(serde_json::json!({"content": "for everyone", "shared": true})),
        ),
    )
    .await;
    assert_eq!(mine["profile_id"].as_str(), Some(pond.amina.as_str()));
    assert!(shared["profile_id"].is_null());
}

#[tokio::test]
async fn a_phone_paired_to_nobody_saves_no_memory_and_sees_none() {
    let pond = pond(false).await;
    let amina = pond.amina.clone();
    remember(&pond, "a1", Some(&amina)).await;

    let (status, _) = send(
        &pond,
        call(
            Method::POST,
            "/api/v1/memories",
            Some(serde_json::json!({"content": "anything"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = send(&pond, call(Method::GET, "/api/v1/memories", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        ids(&body).is_empty(),
        "two members and no evidence: a guest"
    );
}

#[tokio::test]
async fn a_members_phone_lists_no_other_members_conversation() {
    let pond = pond(true).await;
    let (amina, baraka) = (pond.amina.clone(), pond.baraka.clone());
    conversation(&pond, "s-amina", Some(&amina)).await;
    conversation(&pond, "s-baraka", Some(&baraka)).await;

    let (status, body) = send(&pond, call(Method::GET, "/api/v1/sessions", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!ids(&body).contains(&"s-baraka".to_string()));
    assert!(ids(&body).contains(&"s-amina".to_string()));
}

#[tokio::test]
async fn another_members_conversation_cannot_be_deleted_or_continued() {
    let pond = pond(true).await;
    let baraka = pond.baraka.clone();
    conversation(&pond, "s-baraka", Some(&baraka)).await;

    let (status, _) = send(
        &pond,
        call(Method::DELETE, "/api/v1/sessions/s-baraka", None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        pond.sessions.get_session("s-baraka").await.is_ok(),
        "still there"
    );

    for route in [
        "/api/v1/chat",
        "/api/v1/chat/stream",
        "/api/v1/agent/chat/stream",
    ] {
        let (status, body) = send(
            &pond,
            call(
                Method::POST,
                route,
                Some(serde_json::json!({"message": "what did I say?", "session_id": "s-baraka"})),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{route}: {body}");
    }
}
