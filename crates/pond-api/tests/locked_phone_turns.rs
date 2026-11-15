//! A turn from a locked phone is nobody's: it neither continues the phone member's own
//! conversation nor starts work in their name, on either chat route.

#[path = "support/household.rs"]
mod household;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use household::{household_with, Household, JERRY};
use pond_core::models::ports::agent::Agent;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::profile::ProfileScope;
use serde_json::{json, Value};

async fn household_and_agent() -> (Household, Arc<MockAgent>) {
    let agent = Arc::new(MockAgent::new());
    let engine: Arc<dyn Agent> = agent.clone();
    (household_with(|state| state.agent = engine).await, agent)
}

async fn ask(h: &Household, body: Value) -> (StatusCode, Value) {
    h.send(Method::POST, "/api/v1/chat/stream", JERRY, Some(body))
        .await
}

async fn owner_of(h: &Household, session_id: &str) -> Option<String> {
    h.state
        .session_storage
        .get_session_identity(session_id)
        .await
        .unwrap()
        .profile_id
}

#[tokio::test]
async fn a_locked_phone_starts_a_new_conversation_rather_than_continue_its_members() {
    let (h, agent) = household_and_agent().await;
    let (status, _) = ask(
        &h,
        json!({"session_id": "jerrys", "message": "my PIN is 4512"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        owner_of(&h, "jerrys").await.as_deref(),
        Some(h.jerry.as_str())
    );

    let (status, streamed) = ask(
        &h,
        json!({"session_id": "jerrys", "message": "what is my PIN?", "device_locked": true}),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{streamed}");
    let sent = agent.last_request().expect("the turn reached the agent");
    assert_ne!(sent.session_id, "jerrys");
    assert_eq!(sent.profile_scope, ProfileScope::Guest);
    assert!(sent.device_locked);
    assert!(
        streamed.to_string().contains(&sent.session_id),
        "the client is told the conversation it is in: {streamed}"
    );
}

#[tokio::test]
async fn a_locked_phones_own_conversation_stays_nobodys_and_carries_on() {
    let (h, agent) = household_and_agent().await;
    let locked =
        |message: &str| json!({"session_id": "orb", "message": message, "device_locked": true});

    let (status, _) = ask(&h, locked("what's the time?")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(owner_of(&h, "orb").await, None);
    let (status, _) = ask(&h, locked("and the weather?")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(agent.last_request().unwrap().session_id, "orb");
}

#[tokio::test]
async fn a_locked_phone_opens_no_deep_research() {
    let (h, agent) = household_and_agent().await;

    let (status, streamed) = ask(
        &h,
        json!({"message": "research my health records", "deep_research": true, "device_locked": true}),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{streamed}");
    let sent = agent.last_request().expect("the turn reached the agent");
    assert!(sent.deep_research_task.is_none());
    assert!(
        !streamed.to_string().contains("deep research"),
        "{streamed}"
    );
}

#[tokio::test]
async fn the_unstreamed_route_tells_the_model_the_phone_is_locked() {
    let (h, agent) = household_and_agent().await;

    let (status, body) = h
        .send(
            Method::POST,
            "/api/v1/chat",
            JERRY,
            Some(json!({"message": "what's on today?", "device_locked": true})),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let sent = agent.last_request().expect("the turn reached the agent");
    assert!(sent.device_locked);
    assert_eq!(sent.profile_scope, ProfileScope::Guest);
}

#[tokio::test]
async fn the_unstreamed_route_does_not_continue_a_members_conversation_when_locked() {
    let (h, agent) = household_and_agent().await;
    let (status, _) = ask(
        &h,
        json!({"session_id": "jerrys", "message": "my PIN is 4512"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = h
        .send(
            Method::POST,
            "/api/v1/chat",
            JERRY,
            Some(json!({"session_id": "jerrys", "message": "my PIN?", "device_locked": true})),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(body["session_id"], "jerrys");
    assert_ne!(agent.last_request().unwrap().session_id, "jerrys");
}
