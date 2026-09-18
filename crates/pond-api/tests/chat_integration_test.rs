//! Integration tests for POST /api/v1/chat over the full stack: axum router,
//! ChatService, SessionStorage. LLM backend calls are mocked with wiremock, so no
//! real llamafile process is needed.
//! Run: cargo test -p pond-api --test chat_integration_test

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
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
use tower::ServiceExt;

// ── Minimal stubs ──────────────────────────────────────────────────────────────

/// Onboarding always reports completed so the gate never blocks chat.
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

struct MockDeviceRegistry;

#[async_trait::async_trait]
impl DeviceRegistry for MockDeviceRegistry {
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

// ── Test fixture ───────────────────────────────────────────────────────────────

/// Build a test router backed by a real tempdir SQLite database.
/// All chat goes through MockAgent (GooseAdapter in production).
async fn make_app() -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();

    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

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
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(MockDeviceRegistry),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
        lane: None,
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
    (
        build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        tmp,
    )
}

fn chat_request(body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/chat")
        .header("content-type", "application/json")
        .header("Authorization", "Bearer test-token")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn post_chat_returns_echo_via_agent() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .oneshot(chat_request(serde_json::json!({"message": "hello"})))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    // MockAgent echoes "Echo: <input>"
    assert!(
        json["response"].as_str().unwrap_or("").contains("Echo"),
        "expected echo response, got: {}",
        json
    );
    // session_id is present
    assert!(json["session_id"].is_string());
}

#[tokio::test]
async fn post_chat_auto_creates_session() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .oneshot(chat_request(serde_json::json!({"message": "hi"})))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let session_id = json["session_id"]
        .as_str()
        .expect("session_id must be string");
    // Auto-generated sessions are UUIDs (36 chars with hyphens)
    assert_eq!(session_id.len(), 36, "expected UUID-length session_id");
}

#[tokio::test]
async fn post_chat_reuses_provided_session_id() {
    let (app, _tmp) = make_app().await;

    let session_id = "my-known-session";
    let resp = app
        .oneshot(chat_request(serde_json::json!({
            "session_id": session_id,
            "message": "hello"
        })))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json["session_id"], session_id);
}

#[tokio::test]
async fn post_chat_returns_400_for_invalid_json() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/chat")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from("not json at all"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_chat_returns_400_for_missing_message_field() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .oneshot(chat_request(serde_json::json!({"session_id": "s1"})))
        .await
        .unwrap();

    // Missing required `message` field should be rejected.
    assert!(
        resp.status() == StatusCode::BAD_REQUEST
            || resp.status() == StatusCode::UNPROCESSABLE_ENTITY,
        "expected 400 or 422, got {}",
        resp.status()
    );
}

// ── Auth boundary tests ────────────────────────────────────────────────────────

#[tokio::test]
async fn post_chat_returns_401_when_authorization_header_missing() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/chat")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({"message": "hello"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn post_chat_returns_401_for_invalid_token() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/chat")
                .header("content-type", "application/json")
                .header("authorization", "Bearer not-a-real-token")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({"message": "hello"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn health_endpoint_accessible_without_auth() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Persistence regression ─────────────────────────────────────────────────────

/// Regression: chat/stream must persist user and assistant messages to
/// pond_system.db so GET /sessions/{id}/messages can read them. Drain the stream
/// body fully before querying: persistence happens inside the async_stream
/// generator and only runs when polled.
#[tokio::test]
async fn chat_stream_persists_messages_readable_via_sessions_endpoint() {
    let (app, _tmp) = make_app().await;

    let session_id = "stream-persist-test";

    // POST /api/v1/chat/stream — drain the full SSE body so the stream
    // generator runs to completion and both add_message calls execute.
    let stream_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/chat/stream")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "session_id": session_id,
                        "message": "hello"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(stream_resp.status(), StatusCode::OK);
    axum::body::to_bytes(stream_resp.into_body(), usize::MAX)
        .await
        .unwrap();

    // GET /api/v1/sessions/{id}/messages — must return the persisted rows.
    let get_resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/sessions/{}/messages", session_id))
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(get_resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(get_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let messages = json["messages"].as_array().expect("messages array");

    assert_eq!(
        messages.len(),
        2,
        "expected user + assistant message, got: {}",
        json
    );
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "hello");
    assert_eq!(messages[1]["role"], "assistant");
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap_or("")
            .contains("Echo"),
        "assistant message should contain echo response, got: {}",
        messages[1]["content"]
    );
}

// ── Tool-result persistence ────────────────────────────────────────────────────

/// A mock agent that emits one ToolCall + one ToolResult before the final text,
/// so the tool-persistence path in chat_stream is exercised.
struct ToolEmittingMockAgent;

#[async_trait::async_trait]
impl pond_core::models::ports::agent::Agent for ToolEmittingMockAgent {
    async fn chat(
        &self,
        _request: pond_core::models::ports::agent::AgentRequest,
    ) -> anyhow::Result<pond_core::models::ports::agent::AgentResponse> {
        Ok(pond_core::models::ports::agent::AgentResponse {
            text: "Tool done".to_string(),
            metadata: std::collections::HashMap::new(),
        })
    }

    async fn chat_stream(
        &self,
        request: pond_core::models::ports::agent::AgentRequest,
    ) -> anyhow::Result<
        futures::stream::BoxStream<
            'static,
            anyhow::Result<pond_core::models::ports::agent::AgentStreamEvent>,
        >,
    > {
        use futures::StreamExt;
        use pond_core::models::ports::agent::AgentStreamEvent;
        let session_id = request.session_id.clone();
        let model_role = request.model_role.clone();
        let stream = async_stream::stream! {
            yield Ok(AgentStreamEvent::ToolCall {
                id: "tc-1".to_string(),
                tool: "giap__get_weather".to_string(),
                input: Some(serde_json::json!({"location": "Nairobi"})),
            });
            yield Ok(AgentStreamEvent::ToolResult {
                id: "tc-1".to_string(),
                tool: "giap__get_weather".to_string(),
                content: "Sunny, 28°C".to_string(),
            });
            yield Ok(AgentStreamEvent::Text { content: "Tool done".to_string() });
            yield Ok(AgentStreamEvent::Done { session_id, model_role, usage: None, stats: None });
        };
        Ok(stream.boxed())
    }
}

async fn make_app_with_agent(
    agent: Arc<dyn pond_core::models::ports::agent::Agent>,
) -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));
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
        agent,
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(MockDeviceRegistry),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
        lane: None,
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
        event_bus: None,
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_sender: None,
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

/// Regression: tool result rows (role=tool) must be persisted alongside
/// user + assistant rows so the full turn is recoverable from the DB.
#[tokio::test]
async fn chat_stream_persists_tool_result_rows() {
    let (app, _tmp) = make_app_with_agent(Arc::new(ToolEmittingMockAgent)).await;
    let session_id = "tool-persist-test";

    let stream_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/chat/stream")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "session_id": session_id,
                        "message": "what is the weather?"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(stream_resp.status(), StatusCode::OK);
    axum::body::to_bytes(stream_resp.into_body(), usize::MAX)
        .await
        .unwrap();

    let get_resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/sessions/{}/messages", session_id))
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(get_resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(get_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let messages = json["messages"].as_array().expect("messages array");

    assert!(
        messages.len() >= 3,
        "expected at least user + tool + assistant rows, got {}: {}",
        messages.len(),
        json
    );
    assert_eq!(messages[0]["role"], "user");
    let roles: Vec<&str> = messages.iter().filter_map(|m| m["role"].as_str()).collect();
    assert!(
        roles.contains(&"tool"),
        "expected a role=tool row in persisted messages, got roles: {:?}",
        roles
    );
    assert_eq!(messages.last().unwrap()["role"], "assistant");
}

/// A mock agent whose Done event carries TurnStats — verifies the /chat/stream
/// SSE surface emits the `turn_stats` event with the engine numbers.
struct StatsEmittingMockAgent;

#[async_trait::async_trait]
impl pond_core::models::ports::agent::Agent for StatsEmittingMockAgent {
    async fn chat(
        &self,
        request: pond_core::models::ports::agent::AgentRequest,
    ) -> anyhow::Result<pond_core::models::ports::agent::AgentResponse> {
        Ok(pond_core::models::ports::agent::AgentResponse {
            text: format!("Echo: {}", request.message),
            metadata: Default::default(),
        })
    }

    async fn chat_stream(
        &self,
        request: pond_core::models::ports::agent::AgentRequest,
    ) -> anyhow::Result<
        futures::stream::BoxStream<
            'static,
            anyhow::Result<pond_core::models::ports::agent::AgentStreamEvent>,
        >,
    > {
        use futures::StreamExt;
        use pond_core::models::ports::agent::AgentStreamEvent;
        let session_id = request.session_id.clone();
        let model_role = request.model_role.clone();
        let stream = async_stream::stream! {
            yield Ok(AgentStreamEvent::Text { content: "Fast answer".to_string() });
            let mut stats = pond_core::shared::domain::turn_stats::TurnStats {
                ttft_ms: Some(412),
                prefill_ms: Some(2000),
                decode_ms: Some(4000),
                prompt_tokens: 1000,
                completion_tokens: 88,
                context_used_tokens: Some(1000),
                context_limit_tokens: Some(3072),
                inference_count: 1,
                ..Default::default()
            };
            stats.finalize_rates();
            yield Ok(AgentStreamEvent::Done {
                session_id,
                model_role,
                usage: Some(pond_core::models::ports::provider::UsageStats {
                    prompt_tokens: 1000,
                    completion_tokens: 88,
                    reasoning_tokens: Some(240),
                }),
                stats: Some(stats),
            });
        };
        Ok(stream.boxed())
    }
}

/// The SSE stream must surface a `turn_stats` event carrying the engine's
/// per-turn performance numbers when the agent's Done event includes them.
#[tokio::test]
async fn chat_stream_emits_turn_stats_event() {
    let (app, _tmp) = make_app_with_agent(Arc::new(StatsEmittingMockAgent)).await;

    let stream_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/chat/stream")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "session_id": "stats-test",
                        "message": "how fast are you?"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(stream_resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(stream_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&bytes);

    let stats_line = body
        .lines()
        .find(|l| l.contains("\"type\":\"turn_stats\""))
        .unwrap_or_else(|| panic!("no turn_stats SSE event in body: {body}"));
    let payload: serde_json::Value =
        serde_json::from_str(stats_line.trim_start_matches("data: ")).unwrap();
    assert_eq!(payload["ttft_ms"], 412);
    assert_eq!(payload["prompt_tokens"], 1000);
    assert_eq!(payload["completion_tokens"], 88);
    assert_eq!(payload["context_limit_tokens"], 3072);
    assert_eq!(payload["decode_tok_per_sec"], 22.0);
    assert_eq!(payload["inference_count"], 1);
}

/// Truncating a conversation must tell the ENGINE, not only the database: the
/// live engine session holds its own copy of the turns, so deleting rows alone
/// leaves the model reading messages the user removed. Asserted through
/// `MockAgent::forgotten_sessions`, since `Agent::forget_session` defaults to a no-op.
#[tokio::test]
async fn truncating_a_session_also_makes_the_engine_forget_it() {
    let agent = Arc::new(MockAgent::new());
    let (app, _tmp) = make_app_with_agent(agent.clone()).await;

    // One real turn, so there is something to truncate.
    let stream = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/chat/stream")
                .header("Authorization", "Bearer test-token")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    serde_json::json!({"message": "hello", "session_id": "sess-trunc"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    let _ = axum::body::to_bytes(stream.into_body(), usize::MAX)
        .await
        .unwrap();

    let messages = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/sessions/sess-trunc/messages")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(messages.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(messages.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let first_id = body["messages"][0]["id"].as_str().expect("a message id");

    assert!(
        agent.forgotten_sessions().is_empty(),
        "nothing has been truncated yet"
    );

    let deleted = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/sessions/sess-trunc/messages/{first_id}"))
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Status before any other claim: an error payload would satisfy a body
    // predicate just as well as a success.
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    assert_eq!(
        agent.forgotten_sessions(),
        vec!["sess-trunc".to_string()],
        "the rows were deleted but the engine still holds them, so the model \
         keeps seeing the turns the user removed"
    );
}
