//! A sensor reading POSTed through the router is published on a live `InProcessEventBus`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use futures::StreamExt;
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::shared::services::in_process_event_bus::InProcessEventBus;
use pond_core::user_data::mocks::mock_device_registry::MockDeviceRegistry;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::onboarding::SqlxOnboardingRepository;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_skill::SqliteSkillRepository;
use tower::ServiceExt;

/// Router with a real event bus, returned so the test can subscribe.
async fn make_app_with_bus() -> (axum::Router, Arc<InProcessEventBus>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let db = Arc::new(db);

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let bus = Arc::new(InProcessEventBus::new());

    let state = Arc::new(AppState {
        warmup: Default::default(),
        db,
        onboarding_repo: Arc::new(SqlxOnboardingRepository::new(pool.clone())),
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
        device_registry: Arc::new(MockDeviceRegistry),
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
        event_bus: Some(bus.clone() as Arc<dyn EventBus>),
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_sender: None,
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

    let router = build_router(state, std::path::PathBuf::from("pond-desktop/dist"));
    (router, bus, tmp)
}

#[tokio::test]
async fn sensor_post_publishes_to_event_bus() {
    let (app, bus, _tmp) = make_app_with_bus().await;
    let mut subscription = bus.subscribe();

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/sensors")
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "device_id": "backyard-pir",
                "sensor_type": "motion",
                "value": 1.0,
                "unit": "bool",
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "sensor POST should persist"
    );

    let event = tokio::time::timeout(Duration::from_secs(2), subscription.next())
        .await
        .expect("bus event within timeout")
        .expect("a bus event");

    match event {
        BusEvent::Sensor(reading) => {
            assert_eq!(reading.device_id, "backyard-pir");
            assert_eq!(reading.sensor_type, "motion");
        }
        other => panic!("expected BusEvent::Sensor, got {other:?}"),
    }
}
