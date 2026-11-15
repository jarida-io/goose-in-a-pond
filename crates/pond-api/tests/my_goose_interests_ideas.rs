//! `/me/interests`, the interests interview, `/me/portrait` and `/me/ideas` over HTTP, as a
//! paired phone of a household of one calls them.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::user_data::domain::idea::{GooseCan, GooseKind, GoosePlan, Idea, IdeaState};
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_core::user_data::ports::ideas::IdeaRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

/// A model that never answers, so a turn stays `thinking` for as long as a test needs.
struct Silent;

#[async_trait::async_trait]
impl LlmProvider for Silent {
    async fn complete(
        &self,
        _system: &str,
        _messages: Vec<ChatMessage>,
    ) -> anyhow::Result<ChatMessage> {
        std::future::pending().await
    }

    fn model_name(&self) -> String {
        "silent".to_string()
    }
}

/// Answers every interview turn the same way: one note about the last answer and a question.
struct ScriptedInterviewer;

#[async_trait::async_trait]
impl LlmProvider for ScriptedInterviewer {
    async fn complete(
        &self,
        _system: &str,
        messages: Vec<ChatMessage>,
    ) -> anyhow::Result<ChatMessage> {
        let asked = messages
            .first()
            .map(|m| m.content.matches("\nPond: ").count())
            .unwrap_or(0);
        Ok(ChatMessage::assistant(
            json!({
                "notes": if asked == 0 { json!([]) } else { json!([{
                    "topic": "The Boys", "category": "film_tv",
                    "facets": ["Homelander"], "depth": "devoted", "news": "both", "avoid": false
                }]) },
                "text": format!("Question number {}?", asked + 1),
                "choices": ["Yes", "No"], "multi": false, "allow_free": true
            })
            .to_string(),
        ))
    }

    fn model_name(&self) -> String {
        "scripted".to_string()
    }
}

struct Pond {
    router: axum::Router,
    member: String,
    ideas: pond_infra::sqlite_ideas::SqliteIdeaRepository,
    _dir: tempfile::TempDir,
}

async fn a_pond(provider: Option<Arc<dyn LlmProvider>>) -> Pond {
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
    state.llm_provider = Arc::new(tokio::sync::RwLock::new(provider));
    let ideas = pond_infra::sqlite_ideas::SqliteIdeaRepository::new(state.db.system.clone());
    Pond {
        member,
        ideas,
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
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

/// Polls the interview until its turn is written, as the phone does.
async fn until_asking(pond: &Pond) -> Value {
    for _ in 0..100 {
        let (status, body) = call(pond, Method::GET, "/me/interests/interview", json!({})).await;
        assert_eq!(status, StatusCode::OK);
        if body["status"] != "thinking" {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the interview stayed thinking");
}

#[tokio::test]
async fn the_hub_says_interests_and_ideas_are_served() {
    let pond = a_pond(None).await;
    let (status, hub) = call(&pond, Method::GET, "/me/hub", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hub["interests"], true);
    assert_eq!(hub["ideas"], true);
}

#[tokio::test]
async fn an_interest_is_added_listed_changed_and_removed() {
    let pond = a_pond(None).await;
    let (status, row) = call(
        &pond,
        Method::POST,
        "/me/interests",
        json!({"topic": "Formula 1", "category": "sport", "facets": ["Ferrari"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{row}");
    assert_eq!(row["confirmed"], true);
    assert_eq!(row["origin"], "you");
    assert_eq!(row["depth"], "curious");
    assert_eq!(row["news"], "both");
    assert_eq!(row["picture"], Value::Null);
    let id = row["id"].as_str().unwrap().to_string();

    let (_, list) = call(&pond, Method::GET, "/me/interests", json!({})).await;
    assert_eq!(list["interests"].as_array().unwrap().len(), 1);

    let (status, changed) = call(
        &pond,
        Method::PATCH,
        &format!("/me/interests/{id}"),
        json!({"depth": "devoted", "news": "digest"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["depth"], "devoted");
    assert_eq!(changed["news"], "digest");

    let (status, _) = call(&pond, Method::POST, "/me/interests", json!({"topic": "  "})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = call(
        &pond,
        Method::DELETE,
        &format!("/me/interests/{id}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &pond,
        Method::DELETE,
        &format!("/me/interests/{id}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_interview_asks_drafts_reviews_and_saves() {
    let pond = a_pond(Some(Arc::new(ScriptedInterviewer))).await;
    let (status, started) = call(&pond, Method::POST, "/me/interests/interview", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(started["status"], "thinking");

    let asking = until_asking(&pond).await;
    assert_eq!(asking["status"], "asking");
    assert_eq!(asking["asked"], 1);
    assert_eq!(asking["turns"][0]["role"], "goose");
    assert_eq!(asking["turns"][0]["choices"], json!(["Yes", "No"]));

    let (status, thinking) = call(
        &pond,
        Method::POST,
        "/me/interests/interview/answer",
        json!({"choices": ["Yes"], "text": "since season one"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(thinking["status"], "thinking");
    assert_eq!(thinking["turns"][1]["text"], "Yes. since season one");

    let second = until_asking(&pond).await;
    assert_eq!(second["asked"], 2);
    assert_eq!(second["draft"][0]["topic"], "The Boys");
    assert_eq!(second["draft"][0]["id"], "d1");

    let (status, review) = call(
        &pond,
        Method::POST,
        "/me/interests/interview/finish",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(review["status"], "review");

    let (status, saved) = call(
        &pond,
        Method::POST,
        "/me/interests/interview/approve",
        json!({"keep": ["d1"], "edits": {"d1": {"news": "digest"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["saved"], 1);

    let (_, list) = call(&pond, Method::GET, "/me/interests", json!({})).await;
    let row = &list["interests"][0];
    assert_eq!(row["topic"], "The Boys");
    assert_eq!(row["confirmed"], true);
    assert_eq!(row["origin"], "interview");
    assert_eq!(row["news"], "digest");

    let (_, done) = call(&pond, Method::GET, "/me/interests/interview", json!({})).await;
    assert_eq!(done["status"], "done");
}

#[tokio::test]
async fn nothing_can_be_answered_while_the_next_question_is_being_written() {
    let pond = a_pond(Some(Arc::new(Silent))).await;
    let (_, started) = call(&pond, Method::POST, "/me/interests/interview", json!({})).await;
    assert_eq!(started["status"], "thinking");
    let (status, body) = call(
        &pond,
        Method::POST,
        "/me/interests/interview/answer",
        json!({"text": "Gaming"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (_, still) = call(&pond, Method::GET, "/me/interests/interview", json!({})).await;
    assert_eq!(still["status"], "thinking");
}

#[tokio::test]
async fn with_no_model_the_interview_still_asks_a_question() {
    let pond = a_pond(None).await;
    call(&pond, Method::POST, "/me/interests/interview", json!({})).await;
    let asking = until_asking(&pond).await;
    assert_eq!(asking["status"], "asking", "never stuck in thinking");
    assert!(!asking["turns"][0]["text"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn ideas_and_the_portrait_read_empty_before_the_lane_writes_them() {
    let pond = a_pond(None).await;
    let (status, portrait) = call(&pond, Method::GET, "/me/portrait", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{portrait}");
    let (status, ideas) = call(&pond, Method::GET, "/me/ideas", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ideas, json!({"ideas": [], "state": "idle"}));
    let (status, asked) = call(&pond, Method::POST, "/me/ideas/refresh", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(asked["state"], "thinking");
    let (_, ideas) = call(&pond, Method::GET, "/me/ideas", json!({})).await;
    assert_eq!(ideas["state"], "thinking");
    let (status, _) = call(&pond, Method::POST, "/me/ideas/nope/goose", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn taking_an_idea_on_yourself_leaves_goose_out_of_it() {
    let pond = a_pond(None).await;
    pond.ideas
        .add(&Idea {
            id: "idea-1".into(),
            profile_id: pond.member.clone(),
            topic: "Toyota Fielder".into(),
            title: "Catch the January dip on Fielders".into(),
            pitch: "Prices soften after the import rush.".into(),
            why: "You are shopping for a Fielder.".into(),
            grounded_in: vec![],
            sources: vec![],
            picture: None,
            goose_can: Some(GooseCan {
                label: "Watch the listings".into(),
                kind: GooseKind::Watch,
            }),
            goose_plan: GoosePlan::default(),
            you_can: vec!["Set a budget ceiling".into(), "Visit two yards".into()],
            state: IdeaState::New,
            progress: None,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();

    let (status, taken) = call(&pond, Method::POST, "/me/ideas/idea-1/mine", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{taken}");
    assert_eq!(taken["todos"].as_array().unwrap().len(), 2);
    assert_eq!(
        taken["idea"]["state"], "done",
        "working would show Goose as on it"
    );
    let (_, ideas) = call(&pond, Method::GET, "/me/ideas", json!({})).await;
    assert_eq!(ideas["ideas"][0]["state"], "done");
}
