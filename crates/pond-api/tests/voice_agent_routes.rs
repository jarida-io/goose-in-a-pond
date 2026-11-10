//! The Voice agent: what `GET /agents/voice` shows, a member's edit and its reset, what a bad
//! edit or a guest gets, and its prefill.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::shared::ports::agent_prefill::AgentPrefillRegistry as _;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::domain::recipe::AgentRecipe;
use pond_core::user_data::ports::device_attribution::DeviceAttribution as _;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
#[allow(unused_imports)]
use pond_core::user_data::ports::profile::ProfileRepository as _;
use pond_core::user_data::ports::recipe::AgentRecipeRepository;
use pond_infra::sqlite_agent_prefill::SqliteAgentPrefillRegistry;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

struct Pond {
    router: axum::Router,
    member: String,
    recipes: Arc<SqliteRecipeRepository>,
    prefills: SqliteAgentPrefillRegistry,
    agent: Arc<pond_core::shared::mocks::mock_agent::MockAgent>,
    _dir: tempfile::TempDir,
}

/// One member and one phone, which is theirs when `paired`.
async fn pond(paired: bool) -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let agent = Arc::new(pond_core::shared::mocks::mock_agent::MockAgent::new());
    state.agent = agent.clone();
    let member = state
        .profile_repo
        .create(CreateProfileRequest {
            display_name: "Jerry".into(),
            avatar_emoji: "duck".into(),
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
    if paired {
        SqliteDeviceAttribution::new(state.db.system.clone())
            .set_device_profile(&device_id, Some(&member))
            .await
            .unwrap();
    }
    state.handshake = Arc::new(DeviceHandshake(device_id));
    let recipes = Arc::new(SqliteRecipeRepository::new(state.db.system.clone()));
    state.recipe_repo = Some(recipes.clone());
    let prefills = SqliteAgentPrefillRegistry::new(state.db.system.clone());
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        member,
        recipes,
        prefills,
        agent,
        _dir: built.dir,
    }
}

async fn send(pond: &Pond, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
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
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

fn balanced_voice_text() -> String {
    let (template, _) = pond_core::prompts::builtin_template_content("balanced").unwrap();
    pond_core::prompts::voice_mode_body(template)
        .unwrap()
        .to_string()
}

fn group<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["tool_groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["group"] == name)
        .unwrap_or_else(|| panic!("{name} is not offered: {body}"))
}

/// The voice compile runs in the background; it is done when the registry records it.
async fn compiled(pond: &Pond) -> String {
    for _ in 0..100 {
        if let Some(record) = pond.prefills.get_all().await.unwrap().remove("voice") {
            return record.definition_hash;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the Voice agent's prefill was never recorded");
}

#[tokio::test]
async fn shipped_it_is_the_styles_own_text_with_every_group_on() {
    let pond = pond(true).await;
    let (status, body) = send(&pond, Method::GET, "/api/v1/agents/voice", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "voice");
    assert_eq!(body["title"], "Voice");
    assert_eq!(body["edited"], false);
    let shipped = balanced_voice_text();
    assert_eq!(body["shipped_instructions"], shipped.as_str());
    assert_eq!(body["instructions"], shipped.as_str());
    assert_eq!(body["examples"], json!([]));
    assert_eq!(body["max_turns"], Value::Null);
    assert!(body["shipped_max_turns"].as_u64().is_some(), "{body}");
    assert!(body["tool_groups"]
        .as_array()
        .unwrap()
        .iter()
        .all(|g| g["on"] == true && g["label"].as_str().is_some_and(|l| !l.is_empty())));
    assert!(body["tool_groups"]
        .as_array()
        .unwrap()
        .iter()
        .all(|g| g["group"] != "giap-toolkit" && g["group"] != "giap-task-work"));
    assert_eq!(body["prefill"]["agent"], "voice");
    assert_eq!(body["prefill"]["kind"], "default");
    assert_eq!(body["prefill"]["status"], "not_built");

    let (status, prefills) = send(&pond, Method::GET, "/api/v1/agents/prefills", None).await;
    assert_eq!(status, StatusCode::OK, "{prefills}");
    let row = prefills["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["agent"] == "voice")
        .unwrap_or_else(|| panic!("the Voice agent is not listed: {prefills}"));
    assert_eq!(row, &body["prefill"]);
}

#[tokio::test]
async fn a_members_edit_is_kept_out_of_recipes_compiles_and_resets() {
    let pond = pond(true).await;
    let (status, body) = send(
        &pond,
        Method::PUT,
        "/api/v1/agents/voice",
        Some(json!({
            "instructions": "Answer in one sentence.",
            "examples": [{"brief": "What's the weather?", "answer": "Sunny, 24 degrees."}],
            "tool_groups_off": ["giap-weather"],
            "max_turns": 3,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["edited"], true);
    assert_eq!(body["instructions"], "Answer in one sentence.");
    assert_eq!(body["shipped_instructions"], balanced_voice_text().as_str());
    assert_eq!(body["examples"][0]["answer"], "Sunny, 24 degrees.");
    assert_eq!(group(&body, "giap-weather")["on"], false);
    assert_eq!(group(&body, "giap-memory")["on"], true);
    assert_eq!(body["max_turns"], 3);

    let row = pond.recipes.get_by_name("voice").await.unwrap().unwrap();
    assert_eq!(row.template.as_deref(), Some("voice"));
    assert_eq!(row.owner_profile_id.as_deref(), Some(pond.member.as_str()));
    let (_, listed) = send(&pond, Method::GET, "/api/v1/recipes", None).await;
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["name"] != "voice"),
        "{listed}"
    );
    let (status, _) = send(&pond, Method::POST, "/api/v1/recipes/voice/run", None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the row is no runnable recipe"
    );

    let edited_hash = compiled(&pond).await;
    assert_eq!(pond.agent.prewarms(), [true], "the voice shape compiled");
    let (_, body) = send(&pond, Method::GET, "/api/v1/agents/voice", None).await;
    assert_eq!(body["prefill"]["status"], "ready", "{body}");

    // A partial edit keeps the rest.
    let (status, body) = send(
        &pond,
        Method::PUT,
        "/api/v1/agents/voice",
        Some(json!({"max_turns": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["max_turns"], Value::Null);
    assert_eq!(body["instructions"], "Answer in one sentence.");

    let (status, body) = send(&pond, Method::DELETE, "/api/v1/agents/voice", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["edited"], false);
    assert_eq!(body["instructions"], balanced_voice_text().as_str());
    assert!(pond.recipes.get_by_name("voice").await.unwrap().is_none());
    let shipped_role = pond_core::shared::services::voice_agent::VoiceAgent::default()
        .prefill_role(&balanced_voice_text())
        .unwrap();
    let shipped_hash = pond_core::shared::services::agent_prefill::definition_hash(&shipped_role);
    assert_ne!(shipped_hash, edited_hash);
    for _ in 0..100 {
        if compiled(&pond).await == shipped_hash {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the reset never recompiled the shipped voice shape");
}

/// Shipped, a spoken turn thinks as a typed one does (`auto`); a member may turn it on or off.
#[tokio::test]
async fn the_voice_agent_thinks_as_the_household_chooses() {
    let pond = pond(true).await;
    let (_, body) = send(&pond, Method::GET, "/api/v1/agents/voice", None).await;
    assert_eq!(body["thinking"], "auto");
    let (status, body) = send(
        &pond,
        Method::PUT,
        "/api/v1/agents/voice",
        Some(json!({"thinking": "on"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["thinking"], "on");
    assert_eq!(body["edited"], true);
    let (status, body) = send(
        &pond,
        Method::PUT,
        "/api/v1/agents/voice",
        Some(json!({"thinking": "sometimes"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["errors"][0]["path"], "thinking");
    let (_, body) = send(
        &pond,
        Method::PUT,
        "/api/v1/agents/voice",
        Some(json!({"thinking": null})),
    )
    .await;
    assert_eq!(body["thinking"], "auto");
    assert_eq!(body["edited"], false);
}

#[tokio::test]
async fn a_bad_edit_names_every_field_and_changes_nothing() {
    let pond = pond(true).await;
    let examples: Vec<Value> = (0..9)
        .map(|i| json!({"brief": format!("q{i}"), "answer": "a"}))
        .collect();
    let (status, body) = send(
        &pond,
        Method::PUT,
        "/api/v1/agents/voice",
        Some(json!({
            "instructions": "x".repeat(4_001),
            "examples": examples,
            "tool_groups_off": ["giap-weather", "giap-nonsense"],
            "max_turns": 13,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let paths: Vec<&str> = body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    for want in [
        "instructions",
        "examples",
        "tool_groups_off[1]",
        "max_turns",
    ] {
        assert!(paths.contains(&want), "{want} missing from {body}");
    }

    let (status, body) = send(
        &pond,
        Method::PUT,
        "/api/v1/agents/voice",
        Some(json!({"examples": [{"brief": "x", "answer": "y".repeat(2_001)}], "tools": []})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let paths: Vec<&str> = body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["tools"], "{body}");

    let (status, body) = send(
        &pond,
        Method::PUT,
        "/api/v1/agents/voice",
        Some(json!({"examples": [{"brief": "x", "answer": "y".repeat(2_001)}]})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["errors"][0]["path"], "examples[0].answer", "{body}");

    assert!(pond.recipes.get_by_name("voice").await.unwrap().is_none());
    assert!(pond.agent.prewarms().is_empty(), "nothing was compiled");
}

#[tokio::test]
async fn a_guest_may_read_it_and_change_nothing() {
    let pond = pond(false).await;
    let (status, _) = send(&pond, Method::GET, "/api/v1/agents/voice", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &pond,
        Method::PUT,
        "/api/v1/agents/voice",
        Some(json!({"instructions": "Say only yes."})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(&pond, Method::DELETE, "/api/v1/agents/voice", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(pond.recipes.get_by_name("voice").await.unwrap().is_none());
}

#[tokio::test]
async fn the_routes_need_a_token() {
    let pond = pond(true).await;
    for method in [Method::GET, Method::PUT, Method::DELETE] {
        let request = Request::builder()
            .method(method.clone())
            .uri("/api/v1/agents/voice")
            .header("Content-Type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let response = pond.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{method}");
    }
}

#[tokio::test]
async fn the_name_belongs_to_the_shipped_agent() {
    let pond = pond(true).await;
    let (status, body) = send(
        &pond,
        Method::POST,
        "/api/v1/recipes",
        Some(json!({"name": "voice", "yaml": "version: 1.0.0\ntitle: Mine\n"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // A member's agent saved under the name before the Voice agent existed is never overwritten.
    pond.recipes
        .upsert(&AgentRecipe {
            id: "older".into(),
            name: "voice".into(),
            description: String::new(),
            yaml: "version: 1.0.0\ntitle: Mine\n".into(),
            active: true,
            created_at: chrono::Utc::now().to_rfc3339(),
            owner_profile_id: Some(pond.member.clone()),
            template: None,
        })
        .await
        .unwrap();
    let (status, body) = send(
        &pond,
        Method::PUT,
        "/api/v1/agents/voice",
        Some(json!({"instructions": "Be brief."})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (_, body) = send(&pond, Method::GET, "/api/v1/agents/voice", None).await;
    assert_eq!(
        body["edited"], false,
        "its row is not read as the Voice agent"
    );
    let (_, listed) = send(&pond, Method::GET, "/api/v1/recipes", None).await;
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "older"),
        "{listed}"
    );
}

#[tokio::test]
async fn a_rebuild_compiles_the_voice_shape() {
    let pond = pond(true).await;
    let (status, body) = send(
        &pond,
        Method::POST,
        "/api/v1/agents/voice/prefill/rebuild",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    compiled(&pond).await;
    assert_eq!(pond.agent.prewarms(), [true]);
}
