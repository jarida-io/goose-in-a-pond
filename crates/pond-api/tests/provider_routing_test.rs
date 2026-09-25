//! `/api/v1/chat/stream` routes every message through the Agent port; wiremock plays llamafile.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_adapters_llamafile::LlamafileProvider;
use pond_api::{build_router, AppState};
use pond_core::mcp::ports::extension_manager::{
    AddExtensionRequest, ExtensionInfo, ExtensionManagerPort, ToolInfo,
};
use pond_core::models::ports::agent::{Agent, AgentRequest, AgentResponse};
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
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
    // Answering "not onboarded" would make every onboarding write route public.
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

struct StubExtensionManager;

#[async_trait::async_trait]
impl ExtensionManagerPort for StubExtensionManager {
    async fn list_extensions(&self) -> anyhow::Result<Vec<ExtensionInfo>> {
        Ok(vec![ExtensionInfo {
            name: "giap".to_string(),
            kind: "builtin".to_string(),
            description: "GIAP builtin tools".to_string(),
            tools: vec!["giap__get_current_weather".to_string()],
            enabled: true,
            status: "connected".to_string(),
            last_error: None,
        }])
    }

    async fn add_extension(&self, _request: AddExtensionRequest) -> anyhow::Result<ExtensionInfo> {
        anyhow::bail!("not implemented in test")
    }

    async fn remove_extension(&self, _name: &str) -> anyhow::Result<()> {
        anyhow::bail!("not implemented in test")
    }

    async fn list_tools(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec!["giap__get_current_weather".to_string()])
    }

    async fn list_tools_detailed(&self) -> anyhow::Result<Vec<ToolInfo>> {
        Ok(vec![ToolInfo {
            extension: "giap".to_string(),
            name: "get_current_weather".to_string(),
            description: Some("Get the current weather".to_string()),
        }])
    }

    async fn set_enabled(&self, _name: &str, _enabled: bool) -> anyhow::Result<()> {
        Ok(()) // no-op in tests
    }
}

struct ToolCallingAgent;

#[async_trait::async_trait]
impl Agent for ToolCallingAgent {
    async fn chat(&self, _request: AgentRequest) -> anyhow::Result<AgentResponse> {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "tool_calls".to_string(),
            serde_json::json!(["giap__get_current_weather"]).to_string(),
        );
        Ok(AgentResponse {
            text: "Task completed from agent".to_string(),
            metadata,
        })
    }

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> anyhow::Result<
        futures::stream::BoxStream<
            'static,
            anyhow::Result<pond_core::shared::domain::agent::AgentStreamEvent>,
        >,
    > {
        let stream = async_stream::stream! {
            yield Ok(pond_core::shared::domain::agent::AgentStreamEvent::ToolCall {
                id: "test-tool-call-id".to_string(),
                tool: "giap__get_current_weather".to_string(),
                input: None,
            });
            yield Ok(pond_core::shared::domain::agent::AgentStreamEvent::Text { content: "Task completed from agent".to_string() });
            yield Ok(pond_core::shared::domain::agent::AgentStreamEvent::Done {
                session_id: request.session_id,
                model_role: request.model_role,
                usage: None,
                stats: None,
            });
        };
        Ok(futures::stream::StreamExt::boxed(stream))
    }
}

// ── SSE helpers ───────────────────────────────────────────────────────────────

/// Build a minimal OpenAI-compatible SSE body with optional usage in the final chunk.
fn sse_body_with_usage(tokens: &[&str], usage: Option<(u32, u32)>) -> String {
    let mut body = String::new();
    for tok in tokens {
        let escaped = tok.replace('"', "\\\"");
        body.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{escaped}\"}},\"finish_reason\":null}}]}}\n\n"
        ));
    }
    if let Some((prompt, completion)) = usage {
        body.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"\"}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":{prompt},\"completion_tokens\":{completion},\"total_tokens\":{}}}}}\n\n",
            prompt + completion
        ));
    } else {
        body.push_str(
            "data: {\"choices\":[{\"delta\":{\"content\":\"\"},\"finish_reason\":\"stop\"}]}\n\n",
        );
    }
    body.push_str("data: [DONE]\n\n");
    body
}

/// Build an AppState with the given provider wired into llm_provider.
async fn make_app_with_provider(
    provider: Arc<dyn pond_core::models::ports::provider::LlmProvider>,
) -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        transcribe_audio: None,
        session_storage,
        http_client: ReqwestClient::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(Some(provider))),
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

async fn collect_sse_events(body: axum::body::Body) -> Vec<serde_json::Value> {
    use axum::body::to_bytes;
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .collect()
}

fn done_event(events: &[serde_json::Value]) -> Option<&serde_json::Value> {
    events
        .iter()
        .find(|e| e.get("done").and_then(|d| d.as_bool()).unwrap_or(false))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A message classified as "chat" should have model_role = "chat" in the done event.
#[tokio::test]
async fn chat_message_routes_to_chat_provider() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body_with_usage(&["Hello!"], None))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let llamafile = Arc::new(LlamafileProvider::new(Some(&server.uri())));
    let router = llamafile.clone();
    let (app, _tmp) = make_app_with_provider(router).await;

    let resp = app
        .oneshot(stream_request(
            serde_json::json!({"message": "hello there"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let done = done_event(&events).expect("no done event");
    assert_eq!(
        done["model_role"].as_str(),
        Some("chat"),
        "expected model_role=chat, got: {:?}",
        done
    );
}

/// A reasoning-type message goes through the same model (no routing).
#[tokio::test]
async fn think_message_uses_single_provider() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body_with_usage(&["Because light scatters."], None))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let llamafile = Arc::new(LlamafileProvider::new(Some(&server.uri())));
    let router = llamafile.clone();
    let (app, _tmp) = make_app_with_provider(router).await;

    let resp = app
        .oneshot(stream_request(
            serde_json::json!({"message": "explain why the sky is blue"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let done = done_event(&events).expect("no done event");
    assert_eq!(done["model_role"].as_str(), Some("chat"));
}

/// An action/reminder message goes through the same model (no routing).
#[tokio::test]
async fn task_message_uses_single_provider() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body_with_usage(&["Reminder set."], None))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let llamafile = Arc::new(LlamafileProvider::new(Some(&server.uri())));
    let router = llamafile.clone();
    let (app, _tmp) = make_app_with_provider(router).await;

    let resp = app
        .oneshot(stream_request(
            serde_json::json!({"message": "remind me to call mum at 9am"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let done = done_event(&events).expect("no done event");
    assert_eq!(done["model_role"].as_str(), Some("chat"));
}

/// /chat/stream runs via the Agent port, so a 503 from the wired provider does not matter.
#[tokio::test]
async fn provider_failure_does_not_break_universal_agent_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
        .mount(&server)
        .await;

    let llamafile = Arc::new(LlamafileProvider::new(Some(&server.uri())));
    let router = llamafile.clone();
    let (app, _tmp) = make_app_with_provider(router).await;

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "hello"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let error_event = events.iter().find(|e| e.get("error").is_some());
    assert!(
        error_event.is_none(),
        "did not expect an error event when agent path is active, got events: {:?}",
        events
    );

    let text_event = events
        .iter()
        .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("text"));
    assert!(
        text_event.is_some(),
        "expected text event, got events: {:?}",
        events
    );
}

/// The done event must always include a non-empty session_id.
#[tokio::test]
async fn done_event_has_session_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body_with_usage(&["Hi"], None))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let llamafile = Arc::new(LlamafileProvider::new(Some(&server.uri())));
    let router = llamafile.clone();
    let (app, _tmp) = make_app_with_provider(router).await;

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "hi"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let done = done_event(&events).expect("no done event");

    let session_id = done["session_id"].as_str().unwrap_or("");
    assert!(
        !session_id.is_empty(),
        "done event has empty session_id: {:?}",
        done
    );
}

/// Agent-path responses currently emit usage fields with zero values.
#[tokio::test]
async fn done_event_has_usage_shape_for_agent_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body_with_usage(&["Hello!"], Some((12, 47))))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let llamafile = Arc::new(LlamafileProvider::new(Some(&server.uri())));
    let router = llamafile.clone();
    let (app, _tmp) = make_app_with_provider(router).await;

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "hello"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let done = done_event(&events).expect("no done event");

    let prompt = done["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
    let completion = done["usage"]["completion_tokens"].as_u64().unwrap_or(0);
    assert_eq!(
        prompt, 0,
        "expected prompt_tokens=0 in done event: {:?}",
        done
    );
    assert_eq!(
        completion, 0,
        "expected completion_tokens=0 in done event: {:?}",
        done
    );
}

#[tokio::test]
async fn no_provider_still_returns_agent_response_for_non_task_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
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
    let app = build_router(state, std::path::PathBuf::from("pond-desktop/dist"));

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "echo test"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let error_event = events.iter().find(|e| e.get("error").is_some());
    assert!(
        error_event.is_none(),
        "did not expect error event: {:?}",
        events
    );

    let done = done_event(&events).expect("no done event");
    assert_eq!(done["model_role"].as_str(), Some("chat"));
}

#[tokio::test]
async fn task_message_uses_agent_with_tool_call_events_without_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        transcribe_audio: None,
        session_storage,
        http_client: ReqwestClient::new(),
        agent: Arc::new(ToolCallingAgent),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
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
        extension_manager: Some(Arc::new(StubExtensionManager)),
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
    let app = build_router(state, std::path::PathBuf::from("pond-desktop/dist"));

    let resp = app
        .oneshot(stream_request(
            serde_json::json!({"message": "remind me to water plants at 6"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let tool_event = events
        .iter()
        .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("tool_call"));
    assert!(
        tool_event.is_some(),
        "expected tool_call event: {:?}",
        events
    );

    let done = done_event(&events).expect("no done event");
    assert_eq!(done["model_role"].as_str(), Some("chat"));
}
