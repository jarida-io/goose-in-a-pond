//! Per-session token tracking on real SQLite: accumulation, session listing and usage summary.

use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_infra::db::Database;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use std::sync::Arc;

// ── Stubs ──────────────────────────────────────────────────────────────────────

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
            registered_at: "2024-01-01T00:00:00Z".into(),
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

// ── Direct DB Tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn increment_usage_accumulates_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let storage = SqliteSessionStorage::new(db.system);

    storage.create_session("sess-1".into()).await.unwrap();

    storage
        .increment_usage("sess-1", 100, 50, Some("gemma-4"))
        .await
        .unwrap();

    storage
        .increment_usage("sess-1", 120, 80, Some("gemma-4"))
        .await
        .unwrap();

    let session = storage.get_session("sess-1").await.unwrap();
    assert_eq!(session.total_prompt_tokens, 220);
    assert_eq!(session.total_completion_tokens, 130);
    assert_eq!(session.model_name, Some("gemma-4".to_string()));
}

#[tokio::test]
async fn increment_usage_updates_model_name() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let storage = SqliteSessionStorage::new(db.system);

    storage.create_session("sess-2".into()).await.unwrap();

    storage
        .increment_usage("sess-2", 50, 30, Some("gemma-2b"))
        .await
        .unwrap();
    let s1 = storage.get_session("sess-2").await.unwrap();
    assert_eq!(s1.model_name, Some("gemma-2b".to_string()));

    // Switch model mid-session
    storage
        .increment_usage("sess-2", 50, 30, Some("qwen-3b"))
        .await
        .unwrap();
    let s2 = storage.get_session("sess-2").await.unwrap();
    assert_eq!(s2.model_name, Some("qwen-3b".to_string()));
    assert_eq!(s2.total_prompt_tokens, 100);
}

#[tokio::test]
async fn list_sessions_includes_usage_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let storage = SqliteSessionStorage::new(db.system);

    storage.create_session("a".into()).await.unwrap();
    storage.create_session("b".into()).await.unwrap();

    storage
        .increment_usage("a", 200, 100, Some("model-a"))
        .await
        .unwrap();
    storage
        .increment_usage("b", 500, 300, Some("model-b"))
        .await
        .unwrap();

    let sessions = storage.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 2);

    let total_prompt: u32 = sessions.iter().map(|s| s.total_prompt_tokens).sum();
    let total_completion: u32 = sessions.iter().map(|s| s.total_completion_tokens).sum();
    assert_eq!(total_prompt, 700);
    assert_eq!(total_completion, 400);
}

#[tokio::test]
async fn new_session_has_zero_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let storage = SqliteSessionStorage::new(db.system);

    storage.create_session("fresh".into()).await.unwrap();
    let session = storage.get_session("fresh").await.unwrap();
    assert_eq!(session.total_prompt_tokens, 0);
    assert_eq!(session.total_completion_tokens, 0);
    assert_eq!(session.model_name, None);
}

// ── API Integration Tests (via axum router) ──────────────────────────────────

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_infra::mock_handshake::MockHandshake;
use reqwest::Client as ReqwestClient;
use std::collections::HashMap;
use tower::ServiceExt;

async fn make_app() -> (axum::Router, Arc<SqliteSessionStorage>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage: session_storage.clone(),
        http_client: ReqwestClient::new(),
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
        download_tracker: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
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
        face_recognition: None,
        runs: Arc::new(pond_api::runs::RunSupervisor::default()),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
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

    (
        build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        session_storage,
        tmp,
    )
}

fn auth_get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap()
}

async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn api_usage_summary_aggregates_sessions() {
    let (app, storage, _tmp) = make_app().await;

    storage.create_session("s1".into()).await.unwrap();
    storage.create_session("s2".into()).await.unwrap();
    storage
        .increment_usage("s1", 1000, 500, Some("gemma"))
        .await
        .unwrap();
    storage
        .increment_usage("s2", 2000, 1500, Some("qwen"))
        .await
        .unwrap();

    let resp = app
        .oneshot(auth_get("/api/v1/usage/summary"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = json_body(resp).await;
    assert_eq!(json["total_prompt_tokens"].as_u64(), Some(3000));
    assert_eq!(json["total_completion_tokens"].as_u64(), Some(2000));
    assert_eq!(json["total_tokens"].as_u64(), Some(5000));
    assert_eq!(json["session_count"].as_u64(), Some(2));
}

#[tokio::test]
async fn api_list_sessions_includes_tokens() {
    let (app, storage, _tmp) = make_app().await;

    storage.create_session("tk-1".into()).await.unwrap();
    storage
        .increment_usage("tk-1", 300, 150, Some("model-x"))
        .await
        .unwrap();

    let resp = app.oneshot(auth_get("/api/v1/sessions")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = json_body(resp).await;
    let sessions = json["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["total_prompt_tokens"].as_u64(), Some(300));
    assert_eq!(sessions[0]["total_completion_tokens"].as_u64(), Some(150));
    assert_eq!(sessions[0]["model_name"].as_str(), Some("model-x"));
}
