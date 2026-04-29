//! Integration tests for POST /api/v1/chat
//!
//! Tests the full stack: axum router → ChatService → SessionStorage.
//! HTTP calls to the LLM backend are mocked via wiremock — no real
//! llamafile process needed.
//!
//! Run: cargo test -p pond-api --test chat_integration_test

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::domain::onboarding::OnboardingStep;
use pond_core::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::ports::onboarding::OnboardingRepository;
use pond_core::services::mock_agent::MockAgent;
use pond_core::services::mock_memory::MockMemoryRepository;
use pond_core::services::mock_profile::MockProfileRepository;
use pond_core::services::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::services::mock_settings::MockSettingsRepository;
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
    async fn save_step(&self, _: OnboardingStep) -> anyhow::Result<()> { Ok(()) }
    async fn reset(&self) -> anyhow::Result<()> { Ok(()) }
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
        })
    }
    async fn list_devices(&self) -> anyhow::Result<Vec<Device>> { Ok(vec![]) }
    async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> { Ok(None) }
    async fn unregister(&self, _: &str) -> anyhow::Result<()> { Ok(()) }
    async fn heartbeat(&self, _: &str) -> anyhow::Result<()> { Ok(()) }
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
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        session_storage,
        http_client: ReqwestClient::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(MockDeviceRegistry),
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
        speaker_id: None,
        session_user_bindings: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        tool_agent: None,
        answer_reviewer: None,
    });
    (build_router(state, std::path::PathBuf::from("web/dist")), tmp)
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

    let session_id = json["session_id"].as_str().expect("session_id must be string");
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
