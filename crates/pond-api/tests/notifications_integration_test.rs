//! #99 acceptance: a paired device opens `GET /api/v1/notifications/stream` and
//! receives notifications — including ones queued while it was offline. Drives
//! the real router with a live `BroadcastNotificationSender` + offline queue +
//! `SqliteDeviceRegistry` (a device is seeded so the existence check passes).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use futures::StreamExt;
use pond_api::{build_router, AppState};
use pond_core::mcp::ports::notification::Notification;
use pond_core::mcp::ports::notification::NotificationSender;
use pond_core::mcp::ports::notification_queue::NotificationQueueRepository;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{DeviceRegistry, RegisterDeviceRequest};
use pond_infra::broadcast_notification_sender::BroadcastNotificationSender;
use pond_infra::db::Database;
#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;
use pond_infra::onboarding::SqlxOnboardingRepository;
use pond_infra::sqlite_device_registry::SqliteDeviceRegistry;
use pond_infra::sqlite_notification_queue::SqliteNotificationQueue;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_skill::SqliteSkillRepository;
use tower::ServiceExt;

struct Harness {
    router: axum::Router,
    queue: Arc<dyn NotificationQueueRepository>,
    sender: Arc<dyn NotificationSender>,
    device_id: String,
    _tmp: tempfile::TempDir,
}

async fn make_app() -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();

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

    let (notification_tx, _) = tokio::sync::broadcast::channel::<Notification>(64);
    let queue: Arc<dyn NotificationQueueRepository> =
        Arc::new(SqliteNotificationQueue::new(pool.clone()));
    let sender: Arc<dyn NotificationSender> = Arc::new(BroadcastNotificationSender::new(
        notification_tx.clone(),
        queue.clone(),
        None,
    ));

    let db = Arc::new(db);
    let mock_hs = DeviceHandshake(device_id.clone());

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
        device_registry: device_registry.clone(),
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
        event_bus: None,
        event_log: None,
        push_token_repo: None,
        notification_tx,
        notification_queue: Some(queue.clone()),
        notification_sender: Some(sender.clone()),
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

    Harness {
        router: build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        queue,
        sender,
        device_id,
        _tmp: tmp,
    }
}

fn notif(target: &str, title: &str) -> Notification {
    Notification {
        id: test_id(),
        target: target.into(),
        category: "info".into(),
        title: title.into(),
        body: "body".into(),
        timestamp: "2026-06-29T00:00:00Z".into(),
        data: None,
    }
}

/// Monotonic test ids — collision-free, unlike a wall-clock-based generator.
fn test_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("n-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

async fn status_of(router: &axum::Router, uri: &str, auth: bool) -> StatusCode {
    let mut req = Request::builder().method(Method::GET).uri(uri);
    if auth {
        req = req.header("Authorization", "Bearer test-token");
    }
    let resp = router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    resp.status()
}

#[tokio::test]
async fn stream_requires_auth() {
    let h = make_app().await;
    let uri = format!("/api/v1/notifications/stream?device_id={}", h.device_id);
    assert_eq!(
        status_of(&h.router, &uri, false).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn stream_requires_device_id() {
    let h = make_app().await;
    assert_eq!(
        status_of(&h.router, "/api/v1/notifications/stream", true).await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn stream_other_device_is_forbidden_without_disclosing_existence() {
    let h = make_app().await;
    assert_eq!(
        status_of(
            &h.router,
            "/api/v1/notifications/stream?device_id=ghost",
            true
        )
        .await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn stream_flushes_offline_queue_on_connect() {
    let h = make_app().await;

    // A notification arrived while the device was offline (targeted send enqueues).
    h.sender
        .send(notif(&h.device_id, "While you were away"))
        .await
        .unwrap();
    assert_eq!(
        h.queue.list_undelivered(&h.device_id).await.unwrap().len(),
        1,
        "queued while offline"
    );

    // Connect — the first SSE frame should be the flushed notification.
    let uri = format!("/api/v1/notifications/stream?device_id={}", h.device_id);
    let resp = h
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(&uri)
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let mut data = resp.into_body().into_data_stream();
    let frame = tokio::time::timeout(std::time::Duration::from_secs(3), data.next())
        .await
        .expect("an SSE frame within timeout")
        .expect("a body chunk")
        .expect("chunk ok");
    let text = String::from_utf8_lossy(&frame);
    assert!(text.contains("While you were away"), "got: {text}");

    // async-stream runs the post-yield `mark_delivered` only on the next poll;
    // drive it once more (it then awaits live events, so this poll times out —
    // expected) before closing the stream.
    let _ = tokio::time::timeout(std::time::Duration::from_millis(300), data.next()).await;
    drop(data);

    // The flushed notification is now marked delivered.
    assert!(
        h.queue
            .list_undelivered(&h.device_id)
            .await
            .unwrap()
            .is_empty(),
        "marked delivered after flush"
    );
}
