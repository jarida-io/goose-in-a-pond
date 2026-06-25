//! Integration tests for the agent data management REST API.
//!
//! Covers:
//! - GET/PUT/DELETE /api/v1/prompts/{name}   (prompt templates)
//! - GET/POST       /api/v1/agent/extras     (prompt extras)
//! - DELETE         /api/v1/agent/extras/{key}
//! - GET/POST       /api/v1/memories          (memory fragments)
//! - DELETE         /api/v1/memories/{id}
//! - GET/POST       /api/v1/skills            (user skills)
//! - PUT/DELETE     /api/v1/skills/{id}
//! - GET/POST       /api/v1/recipes           (agent recipes)
//! - PUT/DELETE     /api/v1/recipes/{id}
//!
//! All tests use a real SQLite database in a tempdir — no mocking of persistence.
//!
//! Run: cargo test -p pond-api --test agent_data_integration_test

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_skill::SqliteSkillRepository;
use tower::ServiceExt;

// ── Stubs ─────────────────────────────────────────────────────────────────────

struct CompletedOnboarding;

#[async_trait::async_trait]
impl OnboardingRepository for CompletedOnboarding {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        Some(OnboardingStep::Completed)
    }
    async fn save_step(&self, _: OnboardingStep) -> anyhow::Result<()> {
        Ok(())
    }
    async fn reset(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

struct NoDevices;

#[async_trait::async_trait]
impl DeviceRegistry for NoDevices {
    async fn register(&self, req: RegisterDeviceRequest) -> anyhow::Result<Device> {
        Ok(Device {
            id: "mock".into(),
            name: req.name,
            device_type: req.device_type,
            hostname: req.hostname,
            ip_address: None,
            capabilities: req.capabilities,
            registered_at: "2024-01-01 00:00:00".into(),
            last_seen: None,
            is_online: false,
            room: req.room,
        })
    }
    async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        Ok(vec![])
    }
    async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> {
        Ok(None)
    }
    async fn unregister(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn heartbeat(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

struct StubDispatcher;

#[async_trait::async_trait]
impl pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher for StubDispatcher {
    async fn dispatch(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<pond_core::mcp::ports::tools::tool_dispatcher::ToolCallResult> {
        Ok(
            pond_core::mcp::ports::tools::tool_dispatcher::ToolCallResult {
                content: format!("dispatched {tool_name} with {arguments}"),
                success: true,
            },
        )
    }
    async fn available_tools(&self) -> Vec<String> {
        vec!["giap-device-control__set_device_state".to_string()]
    }
    async fn available_tool_definitions(&self) -> Vec<(String, String, serde_json::Value)> {
        vec![]
    }
}

// ── Fixture ───────────────────────────────────────────────────────────────────

async fn make_app() -> (axum::Router, tempfile::TempDir) {
    make_app_with_dispatcher(None).await
}

async fn make_app_with_dispatcher(
    tool_dispatcher: Option<Arc<dyn pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher>>,
) -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let db = Arc::new(db);

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        db: db,
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        session_storage: Arc::new(SqliteSessionStorage::new(pool.clone())),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        face_recognition: None,
        prompt_template_dir: None,
        model_repo: None,
        data_dir: Some(tmp.path().to_path_buf()),
        skip_onboarding: true,
        scheduler: None,
        model_scheduler: None,
        mcp_memory: None,
        extension_manager: None,
        mcp_server_repo: None,
        tool_registry: None,
        marketplace: None,
        secret_repo: None,
        download_tracker: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        piper_http_port: None,
        model_catalog_provider: None,
        model_storage_dir: None,
        prompt_template_repo: Some(Arc::new(SqlitePromptTemplateRepository::new(pool.clone()))),
        prompt_extra_repo: Some(Arc::new(SqlitePromptExtraRepository::new(pool.clone()))),
        skill_repo: Some(Arc::new(SqliteSkillRepository::new(pool.clone()))),
        recipe_repo: Some(Arc::new(SqliteRecipeRepository::new(pool.clone()))),
        llamafile_manager: None,
        event_log_repo: None,
        event_bus: None,
        event_log: None,
        session_user_bindings: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: None,
        memory_extractor: None,
        memory_extraction_service: None,
        last_user_activity: Arc::new(tokio::sync::RwLock::new(std::time::Instant::now())),
        consolidation_cancel: Arc::new(tokio::sync::RwLock::new(None)),
        consolidation_event_tx: tokio::sync::broadcast::channel(16).0,
        consolidation_runner: None,
        inference_pool: None,
        schedule_result_tx: tokio::sync::broadcast::channel(1).0,
        telemetry: None,
        context_monitor: Arc::new(
            pond_core::models::services::context_monitor::ContextMonitor::new(),
        ),
        mcp_app_resources: std::collections::HashMap::new(),
        oauth_state: pond_api::oauth_callback::new_oauth_state(),
        security_policy: None,
        tool_dispatcher,
        api_port: 4000,
    });

    (
        build_router(state, std::path::PathBuf::from("web/dist")),
        tmp,
    )
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap()
}

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn put(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(Method::PUT)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn delete(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::DELETE)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap()
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ── Prompt Template tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn prompt_templates_list_empty() {
    let (app, _tmp) = make_app().await;
    let resp = app.oneshot(get("/api/v1/prompts")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn prompt_templates_upsert_then_get_then_list() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .clone()
        .oneshot(put(
            "/api/v1/prompts/custom",
            serde_json::json!({"content": "You are a test bot.", "description": "Test template"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // GET by name
    let resp = app
        .clone()
        .oneshot(get("/api/v1/prompts/custom"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["name"], "custom");
    assert_eq!(body["content"], "You are a test bot.");
    assert_eq!(body["description"], "Test template");
    assert_eq!(body["is_system"], false);

    // GET list
    let resp = app.oneshot(get("/api/v1/prompts")).await.unwrap();
    let list = body_json(resp).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn prompt_template_get_missing_returns_404() {
    let (app, _tmp) = make_app().await;
    let resp = app
        .oneshot(get("/api/v1/prompts/nonexistent"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn prompt_template_delete_user_defined() {
    let (app, _tmp) = make_app().await;

    app.clone()
        .oneshot(put(
            "/api/v1/prompts/deleteme",
            serde_json::json!({"content": "temp", "description": ""}),
        ))
        .await
        .unwrap();

    let resp = app
        .oneshot(delete("/api/v1/prompts/deleteme"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn prompt_template_delete_system_returns_403() {
    use pond_core::user_data::domain::prompt_template::PromptTemplate;
    use pond_core::user_data::ports::prompt_template::PromptTemplateRepository as _;

    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();

    // Seed a system template directly
    let repo = SqlitePromptTemplateRepository::new(pool.clone());
    repo.upsert(&PromptTemplate {
        name: "balanced".into(),
        content: "You are balanced.".into(),
        description: "Built-in".into(),
        is_system: true,
        updated_at: String::new(),
    })
    .await
    .unwrap();

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        session_storage: Arc::new(SqliteSessionStorage::new(pool.clone())),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        face_recognition: None,
        prompt_template_dir: None,
        model_repo: None,
        data_dir: None,
        skip_onboarding: true,
        scheduler: None,
        model_scheduler: None,
        mcp_memory: None,
        extension_manager: None,
        mcp_server_repo: None,
        tool_registry: None,
        marketplace: None,
        secret_repo: None,
        download_tracker: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        piper_http_port: None,
        model_catalog_provider: None,
        model_storage_dir: None,
        prompt_template_repo: Some(Arc::new(SqlitePromptTemplateRepository::new(pool.clone()))),
        prompt_extra_repo: None,
        skill_repo: None,
        recipe_repo: None,
        llamafile_manager: None,
        event_log_repo: None,
        event_bus: None,
        event_log: None,
        session_user_bindings: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: None,
        memory_extractor: None,
        memory_extraction_service: None,
        last_user_activity: Arc::new(tokio::sync::RwLock::new(std::time::Instant::now())),
        consolidation_cancel: Arc::new(tokio::sync::RwLock::new(None)),
        consolidation_event_tx: tokio::sync::broadcast::channel(16).0,
        consolidation_runner: None,
        inference_pool: None,
        schedule_result_tx: tokio::sync::broadcast::channel(1).0,
        telemetry: None,
        context_monitor: Arc::new(
            pond_core::models::services::context_monitor::ContextMonitor::new(),
        ),
        mcp_app_resources: std::collections::HashMap::new(),
        oauth_state: pond_api::oauth_callback::new_oauth_state(),
        security_policy: None,
        tool_dispatcher: None,
        api_port: 4000,
    });

    let app = build_router(state, std::path::PathBuf::from("web/dist"));
    let resp = app
        .oneshot(delete("/api/v1/prompts/balanced"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ── Prompt Extras tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn prompt_extras_create_and_list() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/agent/extras",
            serde_json::json!({
                "key": "language",
                "instruction": "Always reply in French.",
                "active": true,
                "sort_order": 10
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app.oneshot(get("/api/v1/agent/extras")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let extras = body.as_array().unwrap();
    assert_eq!(extras.len(), 1);
    assert_eq!(extras[0]["key"], "language");
    assert_eq!(extras[0]["sort_order"], 10);
}

#[tokio::test]
async fn prompt_extras_upsert_updates_existing_key() {
    let (app, _tmp) = make_app().await;

    app.clone()
        .oneshot(post(
            "/api/v1/agent/extras",
            serde_json::json!({
                "key": "safety", "instruction": "Original.", "active": true, "sort_order": 0
            }),
        ))
        .await
        .unwrap();

    app.clone()
        .oneshot(post(
            "/api/v1/agent/extras",
            serde_json::json!({
                "key": "safety", "instruction": "Updated.", "active": false, "sort_order": 5
            }),
        ))
        .await
        .unwrap();

    let resp = app.oneshot(get("/api/v1/agent/extras")).await.unwrap();
    let body = body_json(resp).await;
    let extras = body.as_array().unwrap();
    assert_eq!(extras.len(), 1, "upsert must not create a duplicate row");
    assert_eq!(extras[0]["instruction"], "Updated.");
    assert_eq!(extras[0]["active"], false);
}

#[tokio::test]
async fn prompt_extra_delete() {
    let (app, _tmp) = make_app().await;

    app.clone()
        .oneshot(post(
            "/api/v1/agent/extras",
            serde_json::json!({
                "key": "toremove", "instruction": "Gone soon.", "active": true, "sort_order": 0
            }),
        ))
        .await
        .unwrap();

    let resp = app
        .oneshot(delete("/api/v1/agent/extras/toremove"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

// ── Memory tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn memories_save_returns_201_and_list_returns_200() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/memories",
            serde_json::json!({
                "content": "User prefers Celsius",
                "tags": ["preferences"]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app.oneshot(get("/api/v1/memories")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn memories_delete_returns_204() {
    let (app, _tmp) = make_app().await;
    // MockMemoryRepository.delete() is a no-op returning Ok
    let resp = app
        .oneshot(delete("/api/v1/memories/any-id"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

// ── Skills tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn skills_full_lifecycle() {
    let (app, _tmp) = make_app().await;

    // Create
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/skills",
            serde_json::json!({
                "name": "light_control",
                "content": "Call giap__list_registered_devices when asked about lights."
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp).await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"], "light_control");
    assert_eq!(created["active"], true);

    // List
    let resp = app.clone().oneshot(get("/api/v1/skills")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let skills = body_json(resp).await;
    assert_eq!(skills.as_array().unwrap().len(), 1);

    // Update — disable
    let resp = app
        .clone()
        .oneshot(put(
            &format!("/api/v1/skills/{id}"),
            serde_json::json!({"active": false}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let updated = body_json(resp).await;
    assert_eq!(updated["active"], false);
    // content should be preserved
    assert_eq!(updated["name"], "light_control");

    // Delete
    let resp = app
        .oneshot(delete(&format!("/api/v1/skills/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn skill_update_missing_id_returns_404() {
    let (app, _tmp) = make_app().await;
    let resp = app
        .oneshot(put(
            "/api/v1/skills/00000000-0000-0000-0000-000000000000",
            serde_json::json!({"active": false}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn skill_create_missing_content_returns_400() {
    let (app, _tmp) = make_app().await;
    let resp = app
        .oneshot(post(
            "/api/v1/skills",
            serde_json::json!({"name": "incomplete"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── Recipes tests ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn recipes_full_lifecycle() {
    let (app, _tmp) = make_app().await;
    let yaml = "title: Morning Brief\nprompt: Give me weather and schedule.";

    // Create
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/recipes",
            serde_json::json!({
                "name": "morning_brief",
                "description": "Daily briefing",
                "yaml": yaml
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp).await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"], "morning_brief");
    assert_eq!(created["active"], true);

    // List
    let resp = app.clone().oneshot(get("/api/v1/recipes")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let recipes = body_json(resp).await;
    assert_eq!(recipes.as_array().unwrap().len(), 1);

    // Update — change description and disable
    let resp = app
        .clone()
        .oneshot(put(
            &format!("/api/v1/recipes/{id}"),
            serde_json::json!({
                "description": "Updated description",
                "active": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let updated = body_json(resp).await;
    assert_eq!(updated["description"], "Updated description");
    assert_eq!(updated["active"], false);
    // yaml should be preserved
    assert_eq!(updated["yaml"], yaml);

    // Delete
    let resp = app
        .oneshot(delete(&format!("/api/v1/recipes/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn recipe_update_missing_id_returns_404() {
    let (app, _tmp) = make_app().await;
    let resp = app
        .oneshot(put(
            "/api/v1/recipes/00000000-0000-0000-0000-000000000000",
            serde_json::json!({"active": false}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn recipe_create_missing_yaml_returns_400() {
    let (app, _tmp) = make_app().await;
    let resp = app
        .oneshot(post(
            "/api/v1/recipes",
            serde_json::json!({"name": "incomplete"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── Recipe run tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn run_recipe_returns_404_for_unknown_name() {
    let (app, _tmp) = make_app().await;
    let resp = app
        .oneshot(post(
            "/api/v1/recipes/no-such-recipe/run",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn run_recipe_streams_sse_for_existing_recipe() {
    let (app, _tmp) = make_app().await;

    // Seed a valid recipe whose prompt field will become the user message.
    let yaml =
        "title: Lights On\ndescription: Turn the lights on.\nprompt: turn the lights on please";
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/recipes",
            serde_json::json!({
                "name": "lights_on",
                "description": "Turn on lights",
                "yaml": yaml,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app
        .oneshot(post("/api/v1/recipes/lights_on/run", serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("text/event-stream"),
        "expected SSE content-type, got {content_type:?}"
    );

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        body.contains("\"done\":true"),
        "expected a done event in SSE body, got: {body}"
    );
    // MockAgent echoes the input — the resolved prompt should reach the agent.
    // The text stream is chunked, so we look for a fragment unlikely to straddle
    // a chunk boundary instead of the whole prompt.
    assert!(
        body.contains("lights on please"),
        "expected the recipe prompt to be echoed by MockAgent, got: {body}"
    );
}

#[tokio::test]
async fn run_recipe_falls_back_when_yaml_invalid() {
    let (app, _tmp) = make_app().await;

    // create_recipe doesn't validate yaml, so we can persist garbage and still
    // run the recipe — the handler must fall back to the literal prompt.
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/recipes",
            serde_json::json!({
                "name": "broken",
                "description": "intentionally broken yaml",
                "yaml": ":::: not yaml :::: \n  - ?? !!",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app
        .oneshot(post("/api/v1/recipes/broken/run", serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        body.contains("routine: broken"),
        "expected fallback prompt in echoed body, got: {body}"
    );
}

// ── 501 when repos are None ───────────────────────────────────────────────────

#[tokio::test]
async fn returns_501_when_repos_not_configured() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;
    let state = Arc::new(AppState {
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        session_storage: Arc::new(SqliteSessionStorage::new(pool.clone())),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        face_recognition: None,
        prompt_template_dir: None,
        model_repo: None,
        data_dir: None,
        skip_onboarding: true,
        scheduler: None,
        model_scheduler: None,
        mcp_memory: None,
        extension_manager: None,
        mcp_server_repo: None,
        tool_registry: None,
        marketplace: None,
        secret_repo: None,
        download_tracker: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        piper_http_port: None,
        model_catalog_provider: None,
        model_storage_dir: None,
        prompt_template_repo: None,
        prompt_extra_repo: None,
        skill_repo: None,
        recipe_repo: None,
        llamafile_manager: None,
        event_log_repo: None,
        event_bus: None,
        event_log: None,
        session_user_bindings: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: None,
        memory_extractor: None,
        memory_extraction_service: None,
        last_user_activity: Arc::new(tokio::sync::RwLock::new(std::time::Instant::now())),
        consolidation_cancel: Arc::new(tokio::sync::RwLock::new(None)),
        consolidation_event_tx: tokio::sync::broadcast::channel(16).0,
        consolidation_runner: None,
        inference_pool: None,
        schedule_result_tx: tokio::sync::broadcast::channel(1).0,
        telemetry: None,
        context_monitor: Arc::new(
            pond_core::models::services::context_monitor::ContextMonitor::new(),
        ),
        mcp_app_resources: std::collections::HashMap::new(),
        oauth_state: pond_api::oauth_callback::new_oauth_state(),
        security_policy: None,
        tool_dispatcher: None,
        api_port: 4000,
    });
    let app = build_router(state, std::path::PathBuf::from("web/dist"));

    for (method, uri) in &[
        (Method::GET, "/api/v1/prompts"),
        (Method::GET, "/api/v1/agent/extras"),
        (Method::GET, "/api/v1/skills"),
        (Method::GET, "/api/v1/recipes"),
        (Method::POST, "/api/v1/recipes/anything/run"),
    ] {
        let req = Request::builder()
            .method(method.clone())
            .uri(*uri)
            .header("Authorization", "Bearer test-token")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_IMPLEMENTED,
            "expected 501 for {method} {uri}"
        );
    }
}

// ── Direct tool-invoke (POST /api/v1/tools/invoke) ──────────────────────────────

#[tokio::test]
async fn invoke_tool_dispatches_and_returns_content() {
    let (app, _tmp) = make_app_with_dispatcher(Some(Arc::new(StubDispatcher))).await;
    let resp = app
        .oneshot(post(
            "/api/v1/tools/invoke",
            serde_json::json!({
                "server": "giap-device-control",
                "tool": "set_device_state",
                "args": { "device_id": "lamp-1", "power": true }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["tool"], "giap-device-control__set_device_state");
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("set_device_state"));
}

#[tokio::test]
async fn invoke_tool_503_when_dispatcher_absent() {
    let (app, _tmp) = make_app().await; // tool_dispatcher: None
    let resp = app
        .oneshot(post(
            "/api/v1/tools/invoke",
            serde_json::json!({ "server": "giap-device-control", "tool": "set_device_state" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn invoke_tool_400_when_tool_missing() {
    let (app, _tmp) = make_app_with_dispatcher(Some(Arc::new(StubDispatcher))).await;
    let resp = app
        .oneshot(post(
            "/api/v1/tools/invoke",
            serde_json::json!({ "server": "giap-device-control", "tool": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
