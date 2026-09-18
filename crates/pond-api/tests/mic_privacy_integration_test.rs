//! `mic_enabled` end to end, from the HTTP surface to the capture gate. This
//! privacy control once shipped enforced nowhere: the wake-word detector, both
//! capture paths and the barge-in listener all kept opening the device. Payload
//! and round-trip tests cannot see that; only this join can.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use pond_api::{build_router, AppState};
use pond_core::models::domain::mic_gate;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_infra::mock_handshake::MockHandshake;
use reqwest::Client as ReqwestClient;

// ── Minimal mocks (mirrors onboarding_integration_test.rs) ───────────────────

struct MockRepo;

#[async_trait::async_trait]
impl OnboardingRepository for MockRepo {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        Some(OnboardingStep::Completed)
    }
    async fn save_step(&self, _step: OnboardingStep) -> anyhow::Result<()> {
        Ok(())
    }
    async fn reset(&self) -> anyhow::Result<()> {
        Ok(())
    }
    // This fixture is a set-up pond, and since PAI-2 P7 that is what decides
    // whether `PUT /settings` answers a caller with no token. See the token on
    // the requests below.
    async fn is_complete(&self) -> anyhow::Result<bool> {
        Ok(true)
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
            room: req.room,
        })
    }
    async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        Ok(vec![])
    }
    async fn get_device(&self, _id: &str) -> anyhow::Result<Option<Device>> {
        Ok(None)
    }
    async fn unregister(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn heartbeat(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

async fn app() -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage: Arc<dyn pond_core::user_data::ports::session_storage::SessionStorage> =
        Arc::new(pond_infra::sqlite_session_storage::SqliteSessionStorage::new(db.system.clone()));

    // PAI-2 P7: `PUT /settings` stops answering anonymous callers once the pond
    // is set up, and this fixture IS a set-up pond -- `MockRepo` reports
    // `Completed` and `skip_onboarding` is true. The token is what keeps these
    // tests measuring the capture gate rather than the auth middleware.
    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        suggestion_queue: std::sync::Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db: Arc::new(db),
        onboarding_repo: Arc::new(MockRepo),
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

/// The gate is a process global, so these tests would race each other under
/// cargo's default parallel harness — measured: 1-2 failures per run. They take
/// one lock and restore the prior value, so the suite is deterministic whatever
/// else is running in the binary.
static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct GateGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: bool,
}

impl GateGuard {
    fn take() -> Self {
        let lock = GATE.lock().unwrap_or_else(|e| e.into_inner());
        Self {
            _lock: lock,
            saved: mic_gate::mic_enabled(),
        }
    }
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        mic_gate::set_mic_enabled(self.saved);
    }
}

async fn put_mic_enabled(app: &axum::Router, value: bool) -> StatusCode {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/settings")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(format!(r#"{{"mic_enabled": {value}}}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    res.status()
}

/// The whole point: the HTTP surface must reach the capture gate.
#[tokio::test]
async fn turning_the_microphone_off_over_http_closes_the_capture_gate() {
    let _gate = GateGuard::take();
    let (app, _tmp) = app().await;

    assert_eq!(put_mic_enabled(&app, false).await, StatusCode::OK);
    assert!(
        !mic_gate::mic_enabled(),
        "PUT /settings mic_enabled=false must reach the capture gate"
    );
    assert!(
        mic_gate::ensure_mic_enabled().is_err(),
        "capture must be refused while the mic is off"
    );

    assert_eq!(put_mic_enabled(&app, true).await, StatusCode::OK);
    assert!(mic_gate::mic_enabled(), "and back on again");
    assert!(mic_gate::ensure_mic_enabled().is_ok());
}

/// Revoking must take effect on the next capture attempt, not at the next
/// restart. A privacy control you have to reboot to apply is not one.
#[tokio::test]
async fn revocation_does_not_wait_for_a_restart() {
    let _gate = GateGuard::take();
    let (app, _tmp) = app().await;

    mic_gate::set_mic_enabled(true);
    assert!(mic_gate::ensure_mic_enabled().is_ok());

    put_mic_enabled(&app, false).await;

    // No restart, no re-read of settings, no new process.
    assert!(
        mic_gate::ensure_mic_enabled().is_err(),
        "the very next capture attempt must be refused"
    );
}

/// A settings patch that does not mention the microphone must not disturb it.
#[tokio::test]
async fn an_unrelated_settings_patch_leaves_the_microphone_alone() {
    let _gate = GateGuard::take();
    let (app, _tmp) = app().await;

    put_mic_enabled(&app, false).await;
    assert!(!mic_gate::mic_enabled());

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/settings")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(r#"{"assistant_name": "Pond"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        !mic_gate::mic_enabled(),
        "an unrelated patch must not silently re-enable the microphone"
    );
}
