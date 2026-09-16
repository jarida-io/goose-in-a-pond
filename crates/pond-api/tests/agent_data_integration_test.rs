//! Integration tests for the agent data management REST API: the /api/v1 routes for prompt
//! templates, prompt extras, memory fragments, user skills and agent recipes. Every test
//! uses a real SQLite database in a tempdir, with no mocking of persistence.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::session::{IdentificationSource, SessionIdentity};
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_settings::SqliteSettingsRepository;
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
    // PAI-2 P7 made this a required trait method rather than a defaulted one:
    // a default would have to answer from `get_current_step`, and a stub that
    // answers "not onboarded" makes every onboarding write route public
    // wherever it is used. The name of this stub is the answer.
    async fn is_complete(&self) -> anyhow::Result<bool> {
        Ok(true)
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

/// Same app, plus a handle on session storage. Sessions have no create endpoint -- they are
/// born from a chat turn -- so a test about session attribution seeds one through the port.
/// Profiles do have one, so the foreign key is exercised the way production exercises it.
async fn make_app_with_sessions() -> (axum::Router, Arc<SqliteSessionStorage>, tempfile::TempDir) {
    make_app_full(None).await
}

async fn make_app_with_dispatcher(
    tool_dispatcher: Option<Arc<dyn pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher>>,
) -> (axum::Router, tempfile::TempDir) {
    let (router, _storage, tmp) = make_app_full(tool_dispatcher).await;
    (router, tmp)
}

async fn make_app_full(
    tool_dispatcher: Option<Arc<dyn pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher>>,
) -> (axum::Router, Arc<SqliteSessionStorage>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let db = Arc::new(db);

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let session_storage = Arc::new(SqliteSessionStorage::new(pool.clone()));

    let state = Arc::new(AppState {
        warmup: Default::default(),
        db: db,
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage: session_storage.clone(),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        tts_control: None,
        // Real repository, not the mock: these tests assert that deleting the
        // primary member clears the settings row that names them, and an
        // in-memory settings store cannot show that.
        settings_repo: Arc::new(SqliteSettingsRepository::new(pool.clone())),
        // Real repository, not the mock: `sessions.profile_id` is a foreign key
        // into `profiles`, and an in-memory profile store cannot satisfy it.
        profile_repo: Arc::new(SqliteProfileRepository::new(pool.clone())),
        device_registry: Arc::new(NoDevices),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
        account_sync: None,
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
        operational_log: None,
        event_bus: None,
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_sender: None,
        runs: Arc::new(pond_api::runs::RunSupervisor::default()),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: None,
        extraction_status: None,
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
        oauth_outcomes: pond_api::oauth_callback::new_oauth_outcomes(),
        security_policy: None,
        tool_dispatcher,
        api_port: 4000,
        weather_provider: None,
        peer_directory: Arc::new(
            pond_core::mesh::mocks::mock_peer_directory::MockPeerDirectory::new(),
        ),
        credit_ledger: Arc::new(
            pond_core::mesh::mocks::mock_credit_ledger::MockCreditLedger::new(),
        ),
        usage_tally: Arc::new(pond_core::mesh::mocks::mock_usage_tally::MockUsageTally::new()),
        mesh_transport: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_provider: Arc::new(tokio::sync::RwLock::new(None)),
        peer_capability_query: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_rebuild: None,
    });

    (
        build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        session_storage,
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

/// A save returns the saved row, and an omitted description keeps the stored one. The handler
/// must not answer `{"name","status":"ok"}` when the desktop client types it
/// `Promise<PromptTemplate>` and reads `updated.content`, and an omitted `description` must
/// not blank the stored one. Either makes the Prompts tab destructive: the edit vanishes.
#[tokio::test]
async fn saving_a_template_returns_it_and_keeps_the_description() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .clone()
        .oneshot(put(
            "/api/v1/prompts/custom",
            serde_json::json!({"content": "v1", "description": "Written once"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let saved = body_json(resp).await;
    assert_eq!(
        saved["content"], "v1",
        "the save did not return the saved row, so a client that renders the \
         response shows nothing: {saved}"
    );
    assert_eq!(saved["name"], "custom");

    // Exactly what the desktop client sends: content only.
    let resp = app
        .clone()
        .oneshot(put(
            "/api/v1/prompts/custom",
            serde_json::json!({"content": "v2"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let saved = body_json(resp).await;
    assert_eq!(saved["content"], "v2");
    assert_eq!(
        saved["description"], "Written once",
        "omitting `description` cleared it. An absent field means leave it alone, \
         not blank it — the client has never sent one: {saved}"
    );

    let resp = app.oneshot(get("/api/v1/prompts/custom")).await.unwrap();
    let stored = body_json(resp).await;
    assert_eq!(
        stored["description"], "Written once",
        "the response looked right but the row was written blank"
    );
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
        is_customized: false,
        factory_version: pond_core::user_data::domain::prompt_template::FACTORY_VERSION,
        updated_at: String::new(),
    })
    .await
    .unwrap();

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage: Arc::new(SqliteSessionStorage::new(pool.clone())),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
        account_sync: None,
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
        operational_log: None,
        event_bus: None,
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_sender: None,
        runs: Arc::new(pond_api::runs::RunSupervisor::default()),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: None,
        extraction_status: None,
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
        oauth_outcomes: pond_api::oauth_callback::new_oauth_outcomes(),
        security_policy: None,
        tool_dispatcher: None,
        api_port: 4000,
        weather_provider: None,
        peer_directory: Arc::new(
            pond_core::mesh::mocks::mock_peer_directory::MockPeerDirectory::new(),
        ),
        credit_ledger: Arc::new(
            pond_core::mesh::mocks::mock_credit_ledger::MockCreditLedger::new(),
        ),
        usage_tally: Arc::new(pond_core::mesh::mocks::mock_usage_tally::MockUsageTally::new()),
        mesh_transport: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_provider: Arc::new(tokio::sync::RwLock::new(None)),
        peer_capability_query: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_rebuild: None,
    });

    let app = build_router(state, std::path::PathBuf::from("pond-desktop/dist"));
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
                "name": "light-control",
                "content": "Call giap__list_registered_devices when asked about lights."
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp).await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"], "light-control");
    assert_eq!(created["active"], true);
    // No icon supplied — defaults to "sparkles" rather than an empty string.
    assert_eq!(created["icon"], "sparkles");

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
    assert_eq!(updated["name"], "light-control");

    // Update — rename to a human-readable title, and change the description
    let resp = app
        .clone()
        .oneshot(put(
            &format!("/api/v1/skills/{id}"),
            serde_json::json!({"name": "Light Control", "description": "Controls the lights"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let renamed = body_json(resp).await;
    assert_eq!(renamed["name"], "Light Control");
    assert_eq!(renamed["description"], "Controls the lights");
    // active and content should be preserved, untouched by this PUT
    assert_eq!(renamed["active"], false);
    assert_eq!(
        renamed["content"],
        "Call giap__list_registered_devices when asked about lights."
    );
    // icon untouched by this PUT either
    assert_eq!(renamed["icon"], "sparkles");

    // Update — change just the icon
    let resp = app
        .clone()
        .oneshot(put(
            &format!("/api/v1/skills/{id}"),
            serde_json::json!({"icon": "lightbulb"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let recolored = body_json(resp).await;
    assert_eq!(recolored["icon"], "lightbulb");
    // everything else untouched
    assert_eq!(recolored["name"], "Light Control");

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
        warmup: Default::default(),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage: Arc::new(SqliteSessionStorage::new(pool.clone())),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
        account_sync: None,
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
        operational_log: None,
        event_bus: None,
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_sender: None,
        runs: Arc::new(pond_api::runs::RunSupervisor::default()),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: None,
        extraction_status: None,
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
        oauth_outcomes: pond_api::oauth_callback::new_oauth_outcomes(),
        security_policy: None,
        tool_dispatcher: None,
        api_port: 4000,
        weather_provider: None,
        peer_directory: Arc::new(
            pond_core::mesh::mocks::mock_peer_directory::MockPeerDirectory::new(),
        ),
        credit_ledger: Arc::new(
            pond_core::mesh::mocks::mock_credit_ledger::MockCreditLedger::new(),
        ),
        usage_tally: Arc::new(pond_core::mesh::mocks::mock_usage_tally::MockUsageTally::new()),
        mesh_transport: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_provider: Arc::new(tokio::sync::RwLock::new(None)),
        peer_capability_query: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_rebuild: None,
    });
    let app = build_router(state, std::path::PathBuf::from("pond-desktop/dist"));

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

// ── Session identity (PAI-1 P2) ──────────────────────────────────────────────
//
// These go through the router, not the repository: P2 replaced an in-memory map the
// repository layer never saw, so a test below the HTTP boundary proves nothing.

/// Create a household member through the API and return their generated id.
async fn seed_profile(app: &axum::Router, display_name: &str) -> String {
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/profiles",
            serde_json::json!({ "display_name": display_name }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "profile create failed");
    body_json(resp).await["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn session_user_reads_nobody_for_a_session_never_identified() {
    let (app, storage, _tmp) = make_app_with_sessions().await;
    storage.create_session("sess-1".to_string()).await.unwrap();

    let resp = app
        .oneshot(get("/api/v1/sessions/sess-1/user"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["profile_id"], serde_json::Value::Null);
    assert_eq!(body["identification_source"], "unknown");
}

/// The old handler answered from a process-local map, so a restart erased the
/// binding. This asserts the replacement is actually durable: a second router
/// over the same database sees what the first one wrote.
#[tokio::test]
async fn a_binding_survives_the_process_that_made_it() {
    let (app, storage, tmp) = make_app_with_sessions().await;
    storage.create_session("sess-1".to_string()).await.unwrap();
    let jerry = seed_profile(&app, "Jerry").await;
    storage
        .set_session_identity(
            "sess-1",
            &SessionIdentity {
                profile_id: Some(jerry.clone()),
                source: IdentificationSource::Explicit,
                confidence: None,
            },
        )
        .await
        .unwrap();
    drop(storage);

    // A fresh app over the same data dir stands in for a restart.
    let db = Database::init(tmp.path()).await.unwrap();
    let reopened = SqliteSessionStorage::new(db.system.clone());
    let identity = reopened.get_session_identity("sess-1").await.unwrap();
    assert_eq!(identity.profile_id.as_deref(), Some(jerry.as_str()));
    assert_eq!(identity.source, IdentificationSource::Explicit);
}

#[tokio::test]
async fn clearing_a_binding_releases_it_and_reports_whether_there_was_one() {
    let (app, storage, _tmp) = make_app_with_sessions().await;
    storage.create_session("sess-1".to_string()).await.unwrap();
    let jerry = seed_profile(&app, "Jerry").await;
    storage
        .set_session_identity(
            "sess-1",
            &SessionIdentity {
                profile_id: Some(jerry),
                source: IdentificationSource::Face,
                confidence: Some(0.8),
            },
        )
        .await
        .unwrap();

    let body = body_json(
        app.clone()
            .oneshot(delete("/api/v1/sessions/sess-1/user"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["cleared"], true);

    let body = body_json(
        app.clone()
            .oneshot(get("/api/v1/sessions/sess-1/user"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["profile_id"], serde_json::Value::Null);
    assert_eq!(body["identification_source"], "unknown");

    // Idempotent in effect, honest in its report.
    let body = body_json(
        app.oneshot(delete("/api/v1/sessions/sess-1/user"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["cleared"], false);
}

/// The old map accepted any session id, because it was a `HashMap`. Writing to
/// a row that does not exist has to be an error, not a silent success -- an
/// attribution accepted and then discarded is the exact failure PAI-1 exists to
/// end.
#[tokio::test]
async fn clearing_a_binding_on_an_unknown_session_is_404() {
    let (app, _storage, _tmp) = make_app_with_sessions().await;
    let resp = app
        .oneshot(delete("/api/v1/sessions/no-such-session/user"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Reading is deliberately more forgiving than writing: "whose session is
/// this" has a correct answer for a session that does not exist.
#[tokio::test]
async fn reading_the_user_of_an_unknown_session_is_ok_and_says_nobody() {
    let (app, _storage, _tmp) = make_app_with_sessions().await;
    let resp = app
        .oneshot(get("/api/v1/sessions/no-such-session/user"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["profile_id"], serde_json::Value::Null);
}

/// `Explicit` had no producer before this route existed, so the resolution
/// chain could only ever reach its face rung.
#[tokio::test]
async fn a_member_can_say_who_they_are_and_it_sticks() {
    let (app, storage, _tmp) = make_app_with_sessions().await;
    storage.create_session("sess-1".to_string()).await.unwrap();
    let jerry = seed_profile(&app, "Jerry").await;

    let body = body_json(
        app.clone()
            .oneshot(put(
                "/api/v1/sessions/sess-1/user",
                serde_json::json!({ "profile_id": jerry }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["bound"], true);
    assert_eq!(body["identification_source"], "explicit");

    let identity = storage.get_session_identity("sess-1").await.unwrap();
    assert_eq!(identity.profile_id.as_deref(), Some(jerry.as_str()));
    assert_eq!(identity.source, IdentificationSource::Explicit);
    assert_eq!(
        identity.confidence, None,
        "an explicit claim is not a confidence score"
    );
}

/// The strength rule, over HTTP. Somebody typing a name must not displace a
/// device that proved who it was.
#[tokio::test]
async fn saying_who_you_are_cannot_displace_a_paired_device() {
    let (app, storage, _tmp) = make_app_with_sessions().await;
    storage.create_session("sess-1".to_string()).await.unwrap();
    let jerry = seed_profile(&app, "Jerry").await;
    let liz = seed_profile(&app, "Liz").await;

    storage
        .set_session_identity(
            "sess-1",
            &SessionIdentity {
                profile_id: Some(jerry.clone()),
                source: IdentificationSource::PairedDevice,
                confidence: None,
            },
        )
        .await
        .unwrap();

    let body = body_json(
        app.oneshot(put(
            "/api/v1/sessions/sess-1/user",
            serde_json::json!({ "profile_id": liz }),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(body["bound"], false);

    let identity = storage.get_session_identity("sess-1").await.unwrap();
    assert_eq!(
        identity.profile_id.as_deref(),
        Some(jerry.as_str()),
        "the paired-device binding must survive"
    );
    assert_eq!(identity.source, IdentificationSource::PairedDevice);
}

#[tokio::test]
async fn identifying_a_session_that_does_not_exist_is_404() {
    let (app, _storage, _tmp) = make_app_with_sessions().await;
    let resp = app
        .oneshot(put(
            "/api/v1/sessions/no-such-session/user",
            serde_json::json!({ "profile_id": "whoever" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_empty_profile_id_is_rejected() {
    let (app, storage, _tmp) = make_app_with_sessions().await;
    storage.create_session("sess-1".to_string()).await.unwrap();
    let resp = app
        .oneshot(put(
            "/api/v1/sessions/sess-1/user",
            serde_json::json!({ "profile_id": "  " }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── Member deletion (PAI-1 P7) ───────────────────────────────────────────────

#[tokio::test]
async fn deleting_a_member_reports_what_went_and_what_stayed() {
    let (app, storage, _tmp) = make_app_with_sessions().await;
    storage.create_session("sess-1".to_string()).await.unwrap();
    let jerry = seed_profile(&app, "Jerry").await;
    storage
        .set_session_identity(
            "sess-1",
            &SessionIdentity {
                profile_id: Some(jerry.clone()),
                source: IdentificationSource::Explicit,
                confidence: None,
            },
        )
        .await
        .unwrap();

    let body = body_json(
        app.clone()
            .oneshot(delete(&format!("/api/v1/profiles/{jerry}")))
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(body["profile_id"], jerry);
    assert_eq!(body["display_name"], "Jerry");
    // Sessions are RELEASED, never deleted -- a conversation is not solely the
    // speaker's. Reporting it under "deleted" would misdescribe what happened.
    assert_eq!(body["released"]["sessions"], 1);
    // No equality assertion here: the fixture wires MockMemoryRepository, which does not
    // override `count_for_profile` and returns the port default of 0 whatever the state, so
    // asserting 0 would pass against a repository that cannot answer. The real count is
    // covered in pond-infra, against SQL.
    assert!(body["deleted"]["memories"].is_number());

    // and the session itself survived, unattributed
    let identity = storage.get_session_identity("sess-1").await.unwrap();
    assert_eq!(identity.profile_id, None);
    assert!(storage.get_session("sess-1").await.is_ok());
}

/// This used to return 204 for an id that never existed, which made "did I
/// delete the right person" unanswerable.
#[tokio::test]
async fn deleting_a_member_who_does_not_exist_is_404() {
    let (app, _storage, _tmp) = make_app_with_sessions().await;
    let resp = app
        .oneshot(delete("/api/v1/profiles/nobody"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// `settings.primary_profile_id` is a key-value row, not a foreign key, so no
/// cascade can reach it. Deleting the primary member used to leave an id
/// pointing at nobody -- and the single production reader silently got `None`
/// from the lookup, so nothing ever surfaced the dangling reference.
#[tokio::test]
async fn deleting_the_primary_member_clears_the_setting_that_named_them() {
    let (app, _storage, _tmp) = make_app_with_sessions().await;
    let jerry = seed_profile(&app, "Jerry").await;

    let resp = app
        .clone()
        .oneshot(put(
            "/api/v1/settings",
            serde_json::json!({ "primary_profile_id": jerry }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "settings write failed");

    let body = body_json(
        app.clone()
            .oneshot(delete(&format!("/api/v1/profiles/{jerry}")))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["cleared_primary_profile"], true);

    let settings = body_json(app.oneshot(get("/api/v1/settings")).await.unwrap()).await;
    let dangling = settings["primary_profile_id"].as_str().unwrap_or("");
    assert!(
        dangling.is_empty(),
        "primary_profile_id still names a deleted member: {dangling}"
    );
}

/// The counterpart: deleting a NON-primary member must leave the setting alone.
#[tokio::test]
async fn deleting_someone_else_does_not_touch_the_primary_setting() {
    let (app, _storage, _tmp) = make_app_with_sessions().await;
    let jerry = seed_profile(&app, "Jerry").await;
    let liz = seed_profile(&app, "Liz").await;

    app.clone()
        .oneshot(put(
            "/api/v1/settings",
            serde_json::json!({ "primary_profile_id": jerry }),
        ))
        .await
        .unwrap();

    let body = body_json(
        app.clone()
            .oneshot(delete(&format!("/api/v1/profiles/{liz}")))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["cleared_primary_profile"], false);

    let settings = body_json(app.oneshot(get("/api/v1/settings")).await.unwrap()).await;
    assert_eq!(settings["primary_profile_id"], jerry);
}

// ── Profile context follows the speaker (PAI-1 P6) ───────────────────────────

/// Before P6 the built prompt was bound as `_system_prompt` and the adapter
/// passed `None`, so nothing profile-derived reached the model on either engine
/// path. These assert the resolution that now feeds it.
#[tokio::test]
async fn profile_context_follows_the_identified_member_not_the_primary() {
    let (app, storage, _tmp) = make_app_with_sessions().await;
    storage.create_session("sess-1".to_string()).await.unwrap();

    let jerry = seed_profile(&app, "Jerry").await;
    let liz = seed_profile(&app, "Liz").await;
    for (id, name) in [(&jerry, "Jay"), (&liz, "Lizzie")] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri(format!("/api/v1/profiles/{id}"))
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(
                            &serde_json::json!({ "preferences": { "preferred_name": name } }),
                        )
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "preference write failed");
    }

    // Jerry is primary, but Liz is the one talking.
    app.clone()
        .oneshot(put(
            "/api/v1/settings",
            serde_json::json!({ "primary_profile_id": jerry }),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(put(
            "/api/v1/sessions/sess-1/user",
            serde_json::json!({ "profile_id": liz }),
        ))
        .await
        .unwrap();

    // The session resolves to Liz, which is what the prompt will be built from.
    let body = body_json(
        app.oneshot(get("/api/v1/sessions/sess-1/user"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        body["profile_id"], liz,
        "the session must resolve to the member who identified, not the primary one"
    );
    assert_ne!(body["profile_id"], jerry);
}

// ── PAI-1 P4 / PAI-2 P1: the identity-assertion policy ──────────────────────

/// The default is `audit`, and audit must never block. Every other test in this
/// file binds a session without a token and would fail if it did.
#[tokio::test]
async fn identifying_a_session_is_permitted_in_the_default_audit_mode() {
    let (app, storage, _tmp) = make_app_with_sessions().await;
    storage
        .create_session("sess-audit".to_string())
        .await
        .unwrap();
    let profile_id = seed_profile(&app, "Liz").await;

    let resp = app
        .oneshot(put(
            "/api/v1/sessions/sess-audit/user",
            serde_json::json!({ "profile_id": profile_id }),
        ))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "audit mode must record and proceed, never block"
    );
    assert_eq!(body_json(resp).await["bound"], true);
}

/// The gate actually bites: a check nobody has seen fire is indistinguishable from one that
/// cannot. Nothing links a paired device to a member, so no remote caller can prove the
/// identity it asserts, which is why the shipped default is `audit` and not this.
#[tokio::test]
async fn identifying_a_session_is_refused_in_enforce_mode() {
    let (app, storage, _tmp) = make_app_with_sessions().await;
    storage
        .create_session("sess-enforce".to_string())
        .await
        .unwrap();
    let profile_id = seed_profile(&app, "Liz").await;

    let flip = app
        .clone()
        .oneshot(put(
            "/api/v1/settings",
            serde_json::json!({ "security_policy_mode": "enforce" }),
        ))
        .await
        .unwrap();
    assert_eq!(flip.status(), StatusCode::OK, "could not flip the mode");

    let resp = app
        .oneshot(put(
            "/api/v1/sessions/sess-enforce/user",
            serde_json::json!({ "profile_id": profile_id }),
        ))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "enforce mode must refuse an identity the caller cannot prove"
    );

    // And the refusal is real, not cosmetic: the session stayed unattributed.
    let after = storage
        .get_session_identity("sess-enforce")
        .await
        .expect("identity read failed");
    assert_eq!(
        after.profile_id, None,
        "the binding was written despite the refusal"
    );
}

// ── PAI-1 P4 / PAI-2 P1: the policy's first production call site ────────────
// `PUT /sessions/{id}/user` binds a profile_id from the request BODY, so without an ownership
// check any paired device could claim any member's memories. Driven through the router: the
// check sits between the auth middleware, which supplies the Principal, and the handler.

/// The gate bites. Without this test the check is a rule nobody has watched
/// fire — and a `SecurityPolicy` that has never denied anything is exactly the
/// shape of the inert `Ok(true)` this phase exists to replace.
#[tokio::test]
async fn enforce_mode_refuses_an_identity_the_caller_cannot_prove() {
    let (app, storage, _tmp) = make_app_with_sessions().await;
    storage
        .create_session("sess-policy".to_string())
        .await
        .unwrap();
    let liz = seed_profile(&app, "Liz").await;

    let resp = app
        .clone()
        .oneshot(put(
            "/api/v1/settings",
            serde_json::json!({ "security_policy_mode": "enforce" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "could not switch to enforce");

    // The caller holds a valid token and has proved no membership, which is
    // every remote caller today: nothing links a device to a member.
    let resp = app
        .clone()
        .oneshot(put(
            "/api/v1/sessions/sess-policy/user",
            serde_json::json!({ "profile_id": liz }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // ...and the refusal is real, not cosmetic: nobody was bound.
    let resp = app
        .oneshot(get("/api/v1/sessions/sess-policy/user"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["profile_id"], serde_json::Value::Null);
}

/// The shipped default. The SAME assertion the test above refuses must succeed
/// here, or `audit` is silently enforcing and the rollout plan is a fiction.
#[tokio::test]
async fn audit_mode_allows_the_very_assertion_enforce_refuses() {
    let (app, storage, _tmp) = make_app_with_sessions().await;
    storage
        .create_session("sess-audit".to_string())
        .await
        .unwrap();
    let liz = seed_profile(&app, "Liz").await;

    // No mode is set, so this is the default the product ships with.
    let resp = app
        .clone()
        .oneshot(put(
            "/api/v1/sessions/sess-audit/user",
            serde_json::json!({ "profile_id": liz }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "audit mode must not block -- that is the whole point of landing in it"
    );

    // Assert the positive case too. "Not a 403" would also hold if the handler
    // had stopped binding anything at all.
    let resp = app
        .oneshot(get("/api/v1/sessions/sess-audit/user"))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["profile_id"], liz);
}
