//! E2E: "remember X" in one voice turn is recalled in a later one, on the CLI and desktop paths.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::models::mocks::mock_provider::MockProvider;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::shared::mocks::memory_aware_agent::MemoryAwareAgent;
use pond_core::shared::services::chat::ChatService;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_session::InMemorySessionStorage;
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use reqwest::Client as ReqwestClient;
use std::sync::Arc;
use tower::ServiceExt;

// ── Stubs (same as voice_pipeline_integration_test.rs) ───────────────────────

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
            id: "mock".to_string(),
            name: req.name,
            device_type: req.device_type,
            hostname: req.hostname,
            ip_address: None,
            capabilities: req.capabilities,
            registered_at: "2024-01-01 00:00:00".to_string(),
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

// ── Desktop path helpers ──────────────────────────────────────────────────────

async fn make_memory_app() -> (axum::Router, Arc<MockMemoryRepository>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));
    let llm_provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new());

    let memory_repo = Arc::new(MockMemoryRepository::new());
    let agent = Arc::new(MemoryAwareAgent::new(memory_repo.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: String::new(),
        transcribe_audio: None,
        session_storage,
        http_client: ReqwestClient::new(),
        agent,
        llm_provider: Arc::new(tokio::sync::RwLock::new(Some(llm_provider))),
        llamafile_url: String::new(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        matter: None,
        memory_repo: memory_repo.clone(),
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
        event_log: None,
        event_bus: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_sender: None,
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        face_recognition: None,
        runs: Arc::new(pond_api::runs::RunSupervisor::default()),
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
        memory_repo,
        tmp,
    )
}

fn stream_request_with_session(message: &str, session_id: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/chat/stream")
        .header("content-type", "application/json")
        .header("Authorization", "Bearer test-token")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "message": message,
                "session_id": session_id,
            }))
            .unwrap(),
        ))
        .unwrap()
}

async fn collect_text_from_sse(body: axum::body::Body) -> String {
    use axum::body::to_bytes;
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .filter_map(|ev| {
            if ev.get("type").and_then(|t| t.as_str()) == Some("text") {
                ev.get("content")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

// ── Tests: CLI path ───────────────────────────────────────────────────────────

#[tokio::test]
async fn cli_voice_path_cross_turn_memory_recall() {
    let memory_repo = Arc::new(MockMemoryRepository::new());
    let agent = Arc::new(MemoryAwareAgent::new(memory_repo.clone()));
    let storage = Arc::new(InMemorySessionStorage::new());
    let session_id = "cli-voice-memory-test".to_string();
    storage.create_session(session_id.clone()).await.unwrap();

    let service = ChatService::new(agent, session_id.clone(), storage.clone());

    let turn1 = service
        .chat_stream_once("remember my favourite colour is vermillion".to_string())
        .await
        .unwrap();
    assert!(
        turn1.to_lowercase().contains("vermillion"),
        "turn 1 should echo the fact back in its ack; got: {:?}",
        turn1
    );

    let turn2 = service
        .chat_stream_once("what do you remember about me?".to_string())
        .await
        .unwrap();
    assert!(
        turn2.to_lowercase().contains("vermillion"),
        "turn 2 should recall the stored fact; got: {:?}",
        turn2
    );
}

// ── Tests: Desktop path ───────────────────────────────────────────────────────

#[tokio::test]
async fn desktop_voice_path_cross_turn_memory_recall() {
    let (app, _memory_repo, _tmp) = make_memory_app().await;
    let session_id = uuid::Uuid::new_v4().to_string();

    let resp1 = app
        .clone()
        .oneshot(stream_request_with_session(
            "remember my favourite colour is vermillion",
            &session_id,
        ))
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);
    let turn1_text = collect_text_from_sse(resp1.into_body()).await;
    assert!(
        turn1_text.to_lowercase().contains("vermillion"),
        "turn 1 ack should mention the fact; got: {:?}",
        turn1_text
    );

    let resp2 = app
        .oneshot(stream_request_with_session(
            "what do you remember about me?",
            &session_id,
        ))
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let turn2_text = collect_text_from_sse(resp2.into_body()).await;
    assert!(
        turn2_text.to_lowercase().contains("vermillion"),
        "turn 2 should recall the stored fact; got: {:?}",
        turn2_text
    );
}

// ── Live tests (real LLM) ─────────────────────────────────────────────────────
// Placeholders for checking that a real model calls `save_memory` and `recall_memories`.

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_cli_voice_path_cross_turn_memory_recall() {
    todo!("wire real LLM + MCP memory tools");
}

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_desktop_voice_path_cross_turn_memory_recall() {
    todo!("wire real LLM + MCP memory tools");
}
