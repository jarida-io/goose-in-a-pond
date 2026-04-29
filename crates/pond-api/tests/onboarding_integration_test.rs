//! Integration tests — verifies protected routes are blocked before onboarding
//!
//! Run: cargo test -p pond-api --test onboarding_integration_test

use std::sync::Arc;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use tower::ServiceExt;

use pond_core::ports::onboarding::OnboardingRepository;
use pond_core::domain::onboarding::OnboardingStep;
use pond_api::{AppState, build_router};
use pond_core::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::services::mock_agent::MockAgent;
use pond_core::services::mock_memory::MockMemoryRepository;
use pond_core::services::mock_profile::MockProfileRepository;
use pond_core::services::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::services::mock_settings::MockSettingsRepository;
use reqwest::Client as ReqwestClient;
use pond_infra::mock_handshake::MockHandshake;

// ─────────────────────────────────────────────────────────────────
// Minimal mock
// ─────────────────────────────────────────────────────────────────

struct MockRepo {
    step: std::sync::Mutex<Option<OnboardingStep>>,
}

impl MockRepo {
    fn new(step: Option<OnboardingStep>) -> Self {
        Self { step: std::sync::Mutex::new(step) }
    }
}

#[async_trait::async_trait]
impl OnboardingRepository for MockRepo {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        self.step.lock().ok().and_then(|g| *g)
    }

    async fn save_step(&self, step: OnboardingStep) -> anyhow::Result<()> {
        *self.step.lock().unwrap() = Some(step);
        Ok(())
    }

    async fn reset(&self) -> anyhow::Result<()> {
        *self.step.lock().unwrap() = None;
        Ok(())
    }
}

struct MockDeviceRegistry;

#[async_trait::async_trait]
impl DeviceRegistry for MockDeviceRegistry {
    async fn register(&self, req: RegisterDeviceRequest) -> anyhow::Result<Device> {
        Ok(Device {
            id: "mock-id".to_string(),
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
    async fn get_device(&self, _id: &str) -> anyhow::Result<Option<Device>> { Ok(None) }
    async fn unregister(&self, _id: &str) -> anyhow::Result<()> { Ok(()) }
    async fn heartbeat(&self, _id: &str) -> anyhow::Result<()> { Ok(()) }
}

async fn app_with_step(step: Option<OnboardingStep>) -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();

    let session_storage: Arc<dyn pond_core::ports::session_storage::SessionStorage> =
        Arc::new(pond_infra::sqlite_session_storage::SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        db: Arc::new(db),
        onboarding_repo: Arc::new(MockRepo::new(step)) as Arc<dyn OnboardingRepository + Send + Sync>,
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
        skip_onboarding: false,
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

// ─────────────────────────────────────────────────────────────────
// Public routes — must always be accessible
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn health_is_accessible_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn onboard_status_is_accessible_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/onboard/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn system_info_is_accessible_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/system/info").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}

// ─────────────────────────────────────────────────────────────────
// Protected routes — must be blocked before onboarding
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn chat_is_blocked_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(Request::builder().method("POST").uri("/api/v1/chat")
            .header("Authorization", "Bearer test-token")
            .body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn devices_is_blocked_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/devices")
            .header("Authorization", "Bearer test-token")
            .body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn settings_is_blocked_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/settings").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

// ─────────────────────────────────────────────────────────────────
// Protected routes — must be accessible after onboarding
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn chat_is_accessible_after_onboarding() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    let res: Response = app
        .oneshot(Request::builder().method("POST").uri("/api/v1/chat").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn devices_is_accessible_after_onboarding() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/devices").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn settings_is_accessible_after_onboarding() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/settings").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}
