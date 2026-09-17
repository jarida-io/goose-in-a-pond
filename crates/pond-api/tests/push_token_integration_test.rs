//! #95 acceptance: a paired device can register/unregister its push token over
//! `POST/DELETE /api/v1/devices/{id}/push-token`, and the token is persisted.
//! Drives the real router with a live `SqlitePushTokenRepository` +
//! `SqliteDeviceRegistry` (a device is seeded so the existence check passes).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::push_token::PushTokenRepository;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::onboarding::SqlxOnboardingRepository;
use pond_infra::sqlite_device_registry::SqliteDeviceRegistry;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_push_token::SqlitePushTokenRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_skill::SqliteSkillRepository;
use tower::ServiceExt;

/// Build the router with a live push-token repo + device registry, seeding one
/// device. Returns the router, the push-token repo (to assert persistence), the
/// seeded device id, and the tempdir guard.
async fn make_app() -> (
    axum::Router,
    Arc<dyn PushTokenRepository>,
    String,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();

    // Seed a device so the push-token existence check passes.
    let device_registry: Arc<dyn DeviceRegistry + Send + Sync> =
        Arc::new(SqliteDeviceRegistry::new(pool.clone()));
    let device_id = device_registry
        .register(RegisterDeviceRequest {
            id: None,
            name: "Phone".into(),
            device_type: "gotg".into(),
            hostname: None,
            capabilities: vec![],
            room: None,
        })
        .await
        .unwrap()
        .id;

    let push_repo: Arc<dyn PushTokenRepository> =
        Arc::new(SqlitePushTokenRepository::new(pool.clone()));

    let db = Arc::new(db);
    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        suggestion_queue: std::sync::Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
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
        device_registry: device_registry.clone(),
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
        event_bus: None,
        event_log: None,
        push_token_repo: Some(push_repo.clone()),
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
        push_repo,
        device_id,
        tmp,
    )
}

async fn send(
    app: &axum::Router,
    method: Method,
    uri: &str,
    auth: bool,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if auth {
        req = req.header("Authorization", "Bearer test-token");
    }
    let req = match body {
        Some(b) => req
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn register_then_delete_push_token_persists() {
    let (app, repo, device_id, _tmp) = make_app().await;
    let uri = format!("/api/v1/devices/{device_id}/push-token");

    let (status, body) = send(
        &app,
        Method::POST,
        &uri,
        true,
        Some(serde_json::json!({ "token": "ExponentPushToken[abc]", "platform": "expo" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);

    let stored = repo
        .get(&device_id)
        .await
        .unwrap()
        .expect("token persisted");
    assert_eq!(stored.token, "ExponentPushToken[abc]");

    // DELETE removes it.
    let (status, _) = send(&app, Method::DELETE, &uri, true, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(repo.get(&device_id).await.unwrap().is_none());
}

#[tokio::test]
async fn register_push_token_unknown_device_404() {
    let (app, _repo, _device_id, _tmp) = make_app().await;
    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/devices/ghost/push-token",
        true,
        Some(serde_json::json!({ "token": "t", "platform": "fcm" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn register_push_token_bad_platform_400() {
    let (app, _repo, device_id, _tmp) = make_app().await;
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/devices/{device_id}/push-token"),
        true,
        Some(serde_json::json!({ "token": "t", "platform": "telegram" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn register_push_token_oversized_token_400() {
    let (app, _repo, device_id, _tmp) = make_app().await;
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/devices/{device_id}/push-token"),
        true,
        Some(serde_json::json!({ "token": "x".repeat(5000), "platform": "fcm" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Non-printable-ASCII tokens are refused at the boundary: they are never
/// real push tokens, and downstream the relays truncate the token for logging.
#[tokio::test]
async fn register_push_token_non_ascii_token_400() {
    let (app, repo, device_id, _tmp) = make_app().await;
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/devices/{device_id}/push-token"),
        true,
        Some(serde_json::json!({ "token": "日本語のトークンです", "platform": "fcm" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(repo.get(&device_id).await.unwrap().is_none());
}

#[tokio::test]
async fn register_push_token_requires_auth() {
    let (app, _repo, device_id, _tmp) = make_app().await;
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/devices/{device_id}/push-token"),
        false,
        Some(serde_json::json!({ "token": "t", "platform": "fcm" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
