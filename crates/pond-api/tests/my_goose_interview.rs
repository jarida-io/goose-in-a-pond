//! `/me/interview` over HTTP, as a paired phone of a household of one calls it: fixed
//! questions answered by taps, one write-up, and a draft the member keeps or drops.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use serde_json::{json, Value};
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

/// A model that answers every call with the same words and counts the calls.
struct Scripted {
    reply: String,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl LlmProvider for Scripted {
    async fn complete(
        &self,
        _system: &str,
        _messages: Vec<ChatMessage>,
    ) -> anyhow::Result<ChatMessage> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ChatMessage::assistant(self.reply.clone()))
    }

    fn model_name(&self) -> String {
        "scripted".to_string()
    }
}

struct Pond {
    router: axum::Router,
    model: Option<Arc<Scripted>>,
    _dir: tempfile::TempDir,
}

async fn a_pond(reply: Option<&str>) -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    state
        .profile_repo
        .create(CreateProfileRequest {
            display_name: "Jerry".into(),
            avatar_emoji: "x".into(),
        })
        .await
        .unwrap();
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
    let model = reply.map(|reply| {
        Arc::new(Scripted {
            reply: reply.to_string(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        })
    });
    if let Some(model) = &model {
        *state.llm_provider.write().await = Some(model.clone());
    }
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        model,
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

async fn say(pond: &Pond, body: Value) -> Value {
    let (status, interview) = call(pond, Method::POST, "/me/interview/answer", body).await;
    assert_eq!(status, StatusCode::OK, "{interview}");
    interview
}

fn open_question(interview: &Value) -> &str {
    interview["turns"]
        .as_array()
        .and_then(|turns| turns.last())
        .and_then(|turn| turn["text"].as_str())
        .unwrap_or_default()
}

/// The facts in the member's profile, as `(about, sentence)`.
async fn profile(pond: &Pond) -> Vec<(String, String)> {
    let (status, body) = call(pond, Method::GET, "/me/about", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["headings"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|h| h["facts"].as_array().unwrap().clone())
        .map(|f| {
            (
                f["about"].as_str().unwrap_or_default().to_string(),
                f["content"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn an_interview_of_taps_is_drafted_at_once_and_kept_on_approval() {
    let pond = a_pond(None).await;
    let (status, hub) = call(&pond, Method::GET, "/me/hub", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hub["interview"], true);

    let (status, before) = call(&pond, Method::GET, "/me/interview", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(before["status"], "not_started");
    let (status, refused) = call(
        &pond,
        Method::POST,
        "/me/interview/answer",
        json!({"skip": true}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");

    let (status, started) = call(&pond, Method::POST, "/me/interview", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(started["status"], "asking");
    assert_eq!(started["heading"], "You");
    assert_eq!(started["progress"], json!({"done": 0, "total": 8}));
    assert_eq!(started["sections"].as_array().unwrap().len(), 8);
    assert_eq!(open_question(&started), "What do you do most days?");

    let (status, empty) = call(&pond, Method::POST, "/me/interview/answer", json!({})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{empty}");

    let next = say(&pond, json!({"choices": ["Retired"]})).await;
    assert_eq!(open_question(&next), "Who is in your life at home?");
    // Starting again mid-way resumes; it does not go back to the first question.
    let (_, resumed) = call(&pond, Method::POST, "/me/interview", json!({})).await;
    assert_eq!(resumed["progress"]["done"], 1);

    say(&pond, json!({"choices": ["Just me"]})).await;
    let food = say(&pond, json!({"choices": ["Vegetarian", "Allergies"]})).await;
    assert_eq!(open_question(&food), "What are you allergic to?");
    say(&pond, json!({"text": "peanuts"})).await;
    say(&pond, json!({"skip": true})).await; // days
    say(&pond, json!({"skip": true})).await; // fun
    say(&pond, json!({"choices": ["A daily digest"]})).await;
    say(&pond, json!({"choices": ["Politics"]})).await;
    let review = say(&pond, json!({"choices": ["Short and direct"]})).await;

    assert_eq!(
        review["status"], "review",
        "taps alone need no model: {review}"
    );
    assert_eq!(review["news"], "digest");
    let drafted: Vec<(&str, &str)> = review["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["about"].as_str().unwrap(), f["content"].as_str().unwrap()))
        .collect();
    assert_eq!(
        drafted,
        vec![
            ("work", "Jerry is retired."),
            ("family", "Jerry lives alone."),
            ("diet", "Jerry is vegetarian."),
            ("allergy", "Jerry is allergic to peanuts."),
            ("instruction", "Jerry does not want politics brought up."),
            ("style", "Jerry wants answers short and direct."),
        ]
    );
    assert!(
        profile(&pond).await.is_empty(),
        "nothing is kept before approval"
    );

    // They drop the family fact and reword the diet one.
    let (status, kept) = call(
        &pond,
        Method::POST,
        "/me/interview/approve",
        json!({
            "facts": ["f1", "f3", "f4", "f5", "f6"],
            "edits": {"f3": "Jerry is vegetarian but eats eggs."},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(kept, json!({"facts": 5, "interests": 0, "news": true}));

    let facts = profile(&pond).await;
    assert_eq!(facts.len(), 5, "{facts:?}");
    assert!(facts.contains(&("diet".into(), "Jerry is vegetarian but eats eggs.".into())));
    assert!(facts.contains(&("allergy".into(), "Jerry is allergic to peanuts.".into())));
    assert!(!facts.iter().any(|(about, _)| about == "family"));

    let (_, news) = call(&pond, Method::GET, "/me/news/settings", json!({})).await;
    assert_eq!(news["digest"], true, "{news}");
    assert_eq!(news["tiles"], false, "{news}");

    let (_, done) = call(&pond, Method::GET, "/me/interview", json!({})).await;
    assert_eq!(done["status"], "done");
    let (status, twice) = call(
        &pond,
        Method::POST,
        "/me/interview/approve",
        json!({"facts": ["f1"]}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{twice}");

    // A second interview repeats nothing the profile already holds.
    call(&pond, Method::POST, "/me/interview", json!({})).await;
    say(&pond, json!({"choices": ["Retired"]})).await;
    let (status, again) = call(&pond, Method::POST, "/me/interview/finish", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["status"], "review");
    assert_eq!(again["facts"], json!([]), "{again}");
}

#[tokio::test]
async fn typed_answers_are_written_up_by_one_model_call() {
    let pond = a_pond(Some(
        "{\"facts\":[\
         {\"about\":\"work\",\"content\":\"Jerry runs a small hardware shop.\"},\
         {\"about\":\"partner\",\"content\":\"Jerry's partner is Amina.\"},\
         {\"about\":\"likes\",\"content\":\"I love football.\"}],\
         \"interests\":[{\"topic\":\"Arsenal\",\"category\":\"sport\",\"facets\":[\"Saka\"]}]}",
    ))
    .await;
    call(&pond, Method::POST, "/me/interview", json!({})).await;
    say(&pond, json!({"choices": ["Self-employed"]})).await;
    say(&pond, json!({"text": "I run a small hardware shop"})).await;
    say(&pond, json!({"choices": ["Partner or spouse"]})).await;
    say(&pond, json!({"text": "Amina"})).await;
    say(&pond, json!({"skip": true})).await; // food
    say(&pond, json!({"skip": true})).await; // days
    let fun = say(&pond, json!({"choices": ["Sport"]})).await;
    assert!(open_question(&fun).starts_with("Which in particular?"));
    say(&pond, json!({"text": "Arsenal, mostly Saka"})).await;
    say(&pond, json!({"skip": true})).await; // news
    say(&pond, json!({"skip": true})).await; // limits
    let thinking = say(&pond, json!({"skip": true})).await; // style
    assert_eq!(thinking["status"], "thinking", "{thinking}");
    let model = pond.model.as_ref().unwrap();
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no question before the last needed a model"
    );

    let mut review = Value::Null;
    for _ in 0..100 {
        let (_, now) = call(&pond, Method::GET, "/me/interview", json!({})).await;
        if now["status"] == "review" {
            review = now;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(review["status"], "review", "the write-up never landed");
    assert_eq!(model.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let drafted: Vec<&str> = review["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["content"].as_str().unwrap())
        .collect();
    assert_eq!(
        drafted,
        vec![
            "Jerry enjoys sport.",
            "Jerry runs a small hardware shop.",
            "Jerry's partner is Amina.",
        ],
        "the first-person sentence is dropped"
    );
    assert_eq!(review["interests"][0]["topic"], "Arsenal");

    let (status, kept) = call(
        &pond,
        Method::POST,
        "/me/interview/approve",
        json!({"facts": ["f2", "f3"], "interests": ["d1"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(kept, json!({"facts": 2, "interests": 1, "news": false}));
    let (_, interests) = call(&pond, Method::GET, "/me/interests", json!({})).await;
    let topics: Vec<&str> = interests["interests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["topic"].as_str().unwrap())
        .collect();
    assert_eq!(topics, vec!["Arsenal"]);
    assert_eq!(interests["interests"][0]["origin"], "interview");
}
