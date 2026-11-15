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
    phone: String,
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
    state.handshake = Arc::new(DeviceHandshake(device_id.clone()));
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
        phone: device_id,
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

/// Each screen lists its own conversations. The thread the pond writes into is listed for its
/// member, and for nobody else.
#[tokio::test]
async fn the_list_keeps_the_surfaces_asked_for_and_shows_a_member_her_goose_thread() {
    use pond_core::shared::services::goose_thread::GooseThread;
    use pond_core::user_data::domain::message_note::{MessageNote, SourceKind};
    use pond_core::user_data::domain::session::SessionSurface;

    let pond = pond(true).await;
    let (amina, baraka) = (pond.amina.clone(), pond.baraka.clone());
    conversation(&pond, "s-phone", Some(&amina)).await;
    conversation(&pond, "s-room", Some(&amina)).await;
    conversation(&pond, "s-earlier", Some(&amina)).await;
    conversation(&pond, "s-her-other-phone", Some(&amina)).await;
    pond.sessions
        .mark_surface("s-phone", SessionSurface::Phone, Some(&pond.phone))
        .await
        .unwrap();
    pond.sessions
        .mark_surface(
            "s-her-other-phone",
            SessionSurface::Phone,
            Some("her-other-phone"),
        )
        .await
        .unwrap();
    pond.sessions
        .mark_surface("s-room", SessionSurface::Room, None)
        .await
        .unwrap();
    let thread = GooseThread::new(pond.sessions.clone());
    let note = MessageNote::from_source(SourceKind::Schedule, "Daily Briefing", None);
    let hers = thread
        .post(Some(&amina), "Here is your brief.", note.clone())
        .await
        .unwrap();
    let his = thread
        .post(Some(&baraka), "Here is your brief.", note)
        .await
        .unwrap();

    let (status, all) = send(&pond, call(Method::GET, "/api/v1/sessions", None)).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert!(ids(&all).contains(&hers.session_id), "{all}");
    assert!(
        !ids(&all).contains(&his.session_id),
        "another member's thread"
    );

    let listed = |surface: &'static str| {
        let pond = &pond;
        async move {
            let uri = format!("/api/v1/sessions?surface={surface}");
            ids(&send(pond, call(Method::GET, &uri, None)).await.1)
        }
    };
    assert_eq!(listed("phone").await, ["s-her-other-phone", "s-phone"]);
    assert_eq!(
        listed("here,goose").await,
        [hers.session_id.as_str(), "s-phone"],
        "this phone's own and the Goose thread"
    );
    assert_eq!(listed("elsewhere").await, ["s-her-other-phone"]);
    let started_here = |id: &str| {
        all["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .map(|s| s["started_here"].clone())
    };
    assert_eq!(started_here("s-phone"), Some(serde_json::json!(true)));
    assert_eq!(
        started_here("s-her-other-phone"),
        Some(serde_json::json!(false))
    );
    assert!(
        !all.to_string().contains(&pond.phone),
        "no device id is sent"
    );
    assert_eq!(listed("room").await, ["s-room"]);
    assert_eq!(
        listed("unknown").await,
        ["s-earlier"],
        "begun before surfaces were recorded"
    );

    let uri = format!("/api/v1/sessions/{}/messages", hers.session_id);
    let (status, messages) = send(&pond, call(Method::GET, &uri, None)).await;
    assert_eq!(status, StatusCode::OK, "{messages}");
    let rows = messages
        .as_array()
        .or_else(|| messages["messages"].as_array())
        .expect("a list of messages");
    assert_eq!(rows[0]["note"]["from"]["kind"], "schedule", "{messages}");
    assert_eq!(rows[0]["note"]["from"]["label"], "Daily Briefing");
}

/// A suggestion sits in the thread with what Goose noticed and an offer. Taking it hands back
/// the words to send as the member's own turn, and the offer is answered once.
#[tokio::test]
async fn an_offer_in_the_thread_is_taken_once_and_hands_back_its_words() {
    use pond_core::shared::services::goose_thread::GooseThread;
    use pond_core::user_data::domain::message_note::{
        MessageNote, OfferState, SourceKind, SuggestionOffer,
    };

    let pond = pond(true).await;
    let amina = pond.amina.clone();
    let mut note = MessageNote::from_source(SourceKind::Suggestion, "Suggestion", None);
    note.suggestion = Some(SuggestionOffer {
        noticed: "You asked about Naivasha tickets twice this week.".into(),
        action: "Do it".into(),
        prompt: "Check whether Safari Rally tickets are on sale.".into(),
        state: OfferState::Open,
        suggestion_id: None,
    });
    let posted = GooseThread::new(pond.sessions.clone())
        .post(
            Some(&amina),
            "Shall I check whether the tickets are on sale?",
            note,
        )
        .await
        .unwrap();
    let offer = format!(
        "/api/v1/sessions/{}/messages/{}/offer",
        posted.session_id, posted.message_id
    );

    let take = Some(serde_json::json!({"take": true}));
    let (status, taken) = send(&pond, call(Method::POST, &offer, take.clone())).await;
    assert_eq!(status, StatusCode::OK, "{taken}");
    assert_eq!(taken["state"], "done");
    assert_eq!(
        taken["prompt"],
        "Check whether Safari Rally tickets are on sale."
    );

    let (status, again) = send(&pond, call(Method::POST, &offer, take)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{again}");

    let kept = pond
        .sessions
        .get_messages(&posted.session_id)
        .await
        .unwrap();
    let offer = kept[0].note.as_ref().unwrap().suggestion.as_ref().unwrap();
    assert_eq!(offer.state, OfferState::Done);
    assert_eq!(
        offer.noticed,
        "You asked about Naivasha tickets twice this week."
    );
}
