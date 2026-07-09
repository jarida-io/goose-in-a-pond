//! E2E tests: "remember X" in one voice turn → a later turn recalls X.
//!
//! Covers both voice paths:
//! 1. **CLI path** — `ChatService::chat_stream_once()` (the `pond-server chat` CLI)
//! 2. **Desktop path** — `POST /api/v1/chat/stream` SSE endpoint (pond-desktop)
//!
//! Both paths use `MemoryAwareAgent`: a deterministic mock that saves a fact when
//! the message starts with "remember …" and returns stored facts otherwise.
//! No real LLM required.
//!
//! Run: cargo test -p pond-api --test voice_memory_test

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

/// Build an app wired with `MemoryAwareAgent` and a shared `MockMemoryRepository`.
/// Returns `(router, memory_repo, tmp)` so the test can inspect memory directly.
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
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        session_storage,
        http_client: ReqwestClient::new(),
        agent,
        llm_provider: Arc::new(tokio::sync::RwLock::new(Some(llm_provider))),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        memory_repo: memory_repo.clone(),
        embedding_provider: None,
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
        event_log_repo: None,
        event_log: None,
        event_bus: None,
        face_recognition: None,
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
    (
        build_router(state, std::path::PathBuf::from("web/dist")),
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

/// CLI path: "remember X" in turn 1 → turn 2 recalls X.
#[tokio::test]
async fn cli_voice_path_cross_turn_memory_recall() {
    let memory_repo = Arc::new(MockMemoryRepository::new());
    let agent = Arc::new(MemoryAwareAgent::new(memory_repo.clone()));
    let storage = Arc::new(InMemorySessionStorage::new());
    let session_id = "cli-voice-memory-test".to_string();
    storage.create_session(session_id.clone()).await.unwrap();

    let service = ChatService::new(agent, session_id.clone(), storage.clone());

    // Turn 1: ask the agent to remember a fact
    let turn1 = service
        .chat_stream_once("remember my favourite colour is vermillion".to_string())
        .await
        .unwrap();
    assert!(
        turn1.to_lowercase().contains("vermillion"),
        "turn 1 should echo the fact back in its ack; got: {:?}",
        turn1
    );

    // Turn 2: a new utterance should recall the stored fact
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

/// Desktop path: "remember X" in turn 1 → turn 2 recalls X via SSE endpoint.
#[tokio::test]
async fn desktop_voice_path_cross_turn_memory_recall() {
    let (app, _memory_repo, _tmp) = make_memory_app().await;
    let session_id = uuid::Uuid::new_v4().to_string();

    // Turn 1: store a fact
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

    // Turn 2: recall the fact
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
//
// These tests exercise the full stack with a real language model so they can
// verify that the LLM actually calls `save_memory` / `recall_memories` MCP
// tools when instructed.  Run with:
//
//   GIAP_OLLAMA_URL=http://127.0.0.1:11434 GIAP_OLLAMA_MODEL=gemma3:4b \
//     cargo test -p pond-api --test voice_memory_test -- --ignored

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_cli_voice_path_cross_turn_memory_recall() {
    // Placeholder: wire a real GooseAdapter + real MCP memory server and assert recall.
    // Skipped until live test infrastructure is wired to this crate.
    todo!("wire real LLM + MCP memory tools");
}

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_desktop_voice_path_cross_turn_memory_recall() {
    todo!("wire real LLM + MCP memory tools");
}
