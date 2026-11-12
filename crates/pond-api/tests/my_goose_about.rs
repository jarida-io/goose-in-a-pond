//! `/me/about` over HTTP, as a paired phone of a household of one calls it: the member's
//! profile under its headings, their own facts, and what the re-sort set aside.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::user_data::domain::member_profile::{file_under, About};
use pond_core::user_data::domain::memory::{MemoryEventKind, MemoryFragment, MemoryLifecycle};
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

struct Pond {
    router: axum::Router,
    memories: Arc<dyn MemoryRepository + Send + Sync>,
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
    let memories = state.memory_repo.clone();
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        memories,
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
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn add(pond: &Pond, content: &str, about: &str) -> (StatusCode, Value) {
    call(
        pond,
        Method::POST,
        "/me/about",
        json!({"content": content, "about": about}),
    )
    .await
}

/// The headings of the member's profile, each with its facts' words.
async fn headings(pond: &Pond) -> Vec<(String, Vec<String>)> {
    let (status, body) = call(pond, Method::GET, "/me/about", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["headings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["heading"].as_str().unwrap().to_string(),
                h["facts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| f["content"].as_str().unwrap().to_string())
                    .collect(),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_hub_says_the_profile_is_served() {
    let pond = a_pond().await;
    let (status, hub) = call(&pond, Method::GET, "/me/hub", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hub["about"], true);
}

#[tokio::test]
async fn a_profile_is_read_under_its_headings_in_profile_order() {
    let pond = a_pond().await;
    for (content, about) in [
        ("Jerry likes hiking.", "likes"),
        ("Jerry is allergic to peanuts.", "allergy"),
        ("Jerry wants prices in shillings.", "instruction"),
    ] {
        let (status, row) = add(&pond, content, about).await;
        assert_eq!(status, StatusCode::CREATED, "{row}");
    }
    assert_eq!(
        headings(&pond).await,
        vec![
            (
                "Standing instructions".to_string(),
                vec!["Jerry wants prices in shillings.".to_string()]
            ),
            (
                "Food and health".to_string(),
                vec!["Jerry is allergic to peanuts.".to_string()]
            ),
            (
                "Preferences".to_string(),
                vec!["Jerry likes hiking.".to_string()]
            ),
        ]
    );

    let (_, body) = call(&pond, Method::GET, "/me/about", json!({})).await;
    let allergy = &body["headings"][1]["facts"][0];
    assert_eq!(allergy["about"], "allergy");
    assert_eq!(allergy["sensitive"], true);
    assert_eq!(allergy["shared"], false);
    assert_eq!(body["to_file"], 0);
    assert!(body["words"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["about"] == "diet" && w["one_at_a_time"] == true));
}

#[tokio::test]
async fn a_one_at_a_time_fact_replaces_the_old_and_a_repeat_adds_nothing() {
    let pond = a_pond().await;
    let (_, first) = add(&pond, "Jerry is vegetarian.", "diet").await;
    let (status, again) = add(&pond, "jerry is vegetarian", "diet").await;
    assert_eq!(status, StatusCode::OK, "the same fact is not added twice");
    assert_eq!(again["id"], first["id"]);

    let (status, _) = add(&pond, "Jerry eats fish but no other meat.", "diet").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        headings(&pond).await,
        vec![(
            "Food and health".to_string(),
            vec!["Jerry eats fish but no other meat.".to_string()]
        )]
    );
}

#[tokio::test]
async fn a_word_outside_the_list_and_empty_words_are_refused() {
    let pond = a_pond().await;
    let (status, body) = add(&pond, "Jerry likes jazz.", "music taste").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap().contains("allergy"));
    let (status, _) = add(&pond, "   ", "likes").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(headings(&pond).await.is_empty());
}

#[tokio::test]
async fn a_fact_is_reworded_refiled_and_forgotten_by_its_member() {
    let pond = a_pond().await;
    let (_, row) = add(&pond, "Jerry likes hiking.", "likes").await;
    let id = row["id"].as_str().unwrap();

    let (status, edited) = call(
        &pond,
        Method::PATCH,
        &format!("/me/about/{id}"),
        json!({"content": "Jerry hikes every Sunday.", "about": "routine"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["about"], "routine");
    assert_eq!(
        headings(&pond).await,
        vec![(
            "Routines".to_string(),
            vec!["Jerry hikes every Sunday.".to_string()]
        )]
    );

    let (status, _) = call(&pond, Method::DELETE, &format!("/me/about/{id}"), json!({})).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(headings(&pond).await.is_empty());
    let (status, _) = call(&pond, Method::DELETE, &format!("/me/about/{id}"), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_shared_row_is_shown_but_not_the_members_to_change() {
    let pond = a_pond().await;
    let mut shared = MemoryFragment::from_chat(
        "shared-1".into(),
        None,
        None,
        "The household keeps shoes off indoors.".into(),
    );
    file_under(&mut shared, About::Routine);
    pond.memories.add(shared).await.unwrap();

    let (_, body) = call(&pond, Method::GET, "/me/about", json!({})).await;
    assert_eq!(body["headings"][0]["facts"][0]["shared"], true);
    let (status, _) = call(&pond, Method::DELETE, "/me/about/shared-1", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn what_the_re_sort_set_aside_is_listed_with_why_and_comes_back() {
    let pond = a_pond().await;
    let mut old = MemoryFragment::from_chat(
        "old-1".into(),
        Some(pond.member.clone()),
        None,
        "I like my tea strong.".into(),
    );
    old.lifecycle = Some(MemoryLifecycle::Archived);
    pond.memories.add(old).await.unwrap();
    pond.memories
        .log_event(
            MemoryEventKind::Archived,
            "old-1",
            None,
            Some("{\"resort\":\"first person\"}"),
        )
        .await
        .unwrap();
    let mut unfiled = MemoryFragment::from_chat(
        "old-2".into(),
        Some(pond.member.clone()),
        None,
        "Jerry keeps bees.".into(),
    );
    unfiled.lifecycle = Some(MemoryLifecycle::Active);
    pond.memories.add(unfiled).await.unwrap();

    let (_, body) = call(&pond, Method::GET, "/me/about", json!({})).await;
    assert_eq!(body["to_file"], 1, "an older row waits for the re-sort");
    assert_eq!(body["headings"][0]["heading"], "Other");

    let (status, archived) = call(&pond, Method::GET, "/me/about/archived", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    assert_eq!(archived["facts"][0]["content"], "I like my tea strong.");
    assert_eq!(archived["facts"][0]["why"], "first person");

    let (status, _) = call(&pond, Method::POST, "/me/about/old-2/restore", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "a live row is not restored");
    let (status, row) = call(&pond, Method::POST, "/me/about/old-1/restore", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{row}");
    let (_, archived) = call(&pond, Method::GET, "/me/about/archived", json!({})).await;
    assert!(archived["facts"].as_array().unwrap().is_empty());
}
