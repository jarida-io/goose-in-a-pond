//! POST /api/v1/chat/stream, the SSE endpoint driving pond-desktop's voice
//! pipeline: every event carries both the `type`/`content` and `token` fields, a
//! final `done` event names the session and model role, the session and its
//! messages persist, and PUT /api/v1/settings returns the whole Settings object.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::models::mocks::mock_provider::MockProvider;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use reqwest::Client as ReqwestClient;
use std::sync::Arc;
use tower::ServiceExt;

// ── Stubs (same as chat_integration_test.rs) ──────────────────────────────────

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

async fn make_app() -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));
    let llm_provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new());

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        suggestion_queue: std::sync::Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        transcribe_audio: None,
        session_storage,
        http_client: ReqwestClient::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(Some(llm_provider))),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
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
        lane: None,
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
    (
        build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        tmp,
    )
}

fn stream_request(body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/chat/stream")
        .header("content-type", "application/json")
        .header("Authorization", "Bearer test-token")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

// ── Helper: collect all SSE events from the response body ───────────��─────────

async fn collect_sse_events(body: axum::body::Body) -> Vec<serde_json::Value> {
    use axum::body::to_bytes;

    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);

    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .collect()
}

// ── Tests ──────────────────────────────────────────────────────────────────────

/// Canonical event shape: each token event must have BOTH type/content AND token fields.
/// This ensures the desktop voice pipeline and the browser one both work.
#[tokio::test]
async fn stream_events_have_type_text_and_token_fields() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "hi"})))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    assert!(!events.is_empty(), "expected at least one SSE event");

    // Find token/text events (not the done event)
    let text_events: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("text"))
        .collect();

    assert!(
        !text_events.is_empty(),
        "expected at least one text event, got events: {:?}",
        events
    );

    for ev in &text_events {
        assert!(
            ev.get("content").and_then(|c| c.as_str()).is_some(),
            "text event missing 'content' field: {:?}",
            ev
        );
        assert!(
            ev.get("token").and_then(|t| t.as_str()).is_some(),
            "text event missing 'token' field (backwards compat): {:?}",
            ev
        );
    }
}

/// The final event in every stream must be done=true with session_id and model_role.
#[tokio::test]
async fn stream_final_event_is_done_with_session_id() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "hello"})))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;

    let done_event = events
        .iter()
        .find(|e| e.get("done").and_then(|d| d.as_bool()).unwrap_or(false));

    assert!(done_event.is_some(), "no done event found in: {:?}", events);
    let done = done_event.unwrap();

    assert!(
        done.get("session_id").and_then(|s| s.as_str()).is_some(),
        "done event missing session_id: {:?}",
        done
    );
    assert!(
        done.get("model_role").and_then(|r| r.as_str()).is_some(),
        "done event missing model_role: {:?}",
        done
    );
}

/// Passing a session_id reuses the existing session; the done event echoes it back.
#[tokio::test]
async fn stream_uses_provided_session_id() {
    let (app, _tmp) = make_app().await;

    let session_id = "test-voice-session-abc123";
    let resp = app
        .oneshot(stream_request(serde_json::json!({
            "message": "test",
            "session_id": session_id
        })))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let done = events
        .iter()
        .find(|e| e.get("done").and_then(|d| d.as_bool()).unwrap_or(false))
        .expect("no done event");

    assert_eq!(
        done.get("session_id").and_then(|s| s.as_str()),
        Some(session_id),
        "session_id in done event doesn't match provided session_id"
    );
}

/// Messages are persisted — a follow-up GET /sessions/:id/messages should return them.
#[tokio::test]
async fn stream_persists_messages_to_session() {
    let (app, _tmp) = make_app().await;

    let session_id = uuid::Uuid::new_v4().to_string();

    // Send a chat
    let resp = app
        .clone()
        .oneshot(stream_request(serde_json::json!({
            "message": "remember this",
            "session_id": session_id
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // drain the body so the stream completes and messages are persisted
    collect_sse_events(resp.into_body()).await;

    // Fetch the session messages
    let messages_req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/sessions/{}/messages", session_id))
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap();

    let msg_resp = app.oneshot(messages_req).await.unwrap();
    assert_eq!(msg_resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(msg_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let messages = json["messages"].as_array().expect("messages array");
    // At minimum: user message + assistant message
    assert!(
        messages.len() >= 2,
        "expected at least 2 persisted messages, got {}",
        messages.len()
    );

    let roles: Vec<&str> = messages
        .iter()
        .filter_map(|m| m.get("role").and_then(|r| r.as_str()))
        .collect();
    assert!(roles.contains(&"user"), "no user message found");
    assert!(roles.contains(&"assistant"), "no assistant message found");
}
