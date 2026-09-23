//! #164 follow-up: pairing outcomes reach connected devices as security
//! notifications and land in the unified event log under category `Auth`, as
//! `auth.pairing_verify_failed` or `auth.device_paired`. Failure alerts are
//! debounced process-wide to one per 10 minutes, so this binary has one such test.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::mcp::ports::notification::Notification;
use pond_core::mcp::ports::notification::NotificationSender;
use pond_core::mcp::ports::notification_queue::NotificationQueueRepository;
use pond_core::security::domain::event::{
    AttributeValue, EventCategory, EventQuery, PrivacySensitivity,
};
use pond_core::security::ports::event_log::EventLog;
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, VerifyRequest,
};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::mocks::mock_device_registry::MockDeviceRegistry;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_infra::broadcast_notification_sender::BroadcastNotificationSender;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::onboarding::SqlxOnboardingRepository;
use pond_infra::sqlite_event_log::SqliteEventLog;
use pond_infra::sqlite_notification_queue::SqliteNotificationQueue;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_skill::SqliteSkillRepository;
use tower::ServiceExt;

/// Handshake stub that rejects cleanly, the way a wrong code actually does:
/// `Ok(HandshakeResponse { accepted: false, rejection_reason: Some(..) })`,
/// which is a 200 carrying a refusal rather than the 500 `MockHandshake` gives.
/// This is the path an operator hits while pairing, so it is the one whose
/// reason has to survive into the record.
struct RejectingHandshake;

#[async_trait]
impl Handshake for RejectingHandshake {
    async fn handshake(&self, _request: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("not used in this test")
    }
    async fn validate_token(&self, _token: &str) -> Result<bool> {
        Ok(false)
    }
    async fn revoke_device(&self, _device_id: &str) -> Result<u64> {
        Ok(0)
    }
    async fn revoke_token(&self, _token: &str) -> Result<()> {
        Ok(())
    }
    async fn verify_handshake(&self, _request: VerifyRequest) -> Result<HandshakeResponse> {
        Ok(HandshakeResponse {
            accepted: false,
            session_token: None,
            refresh_token: None,
            expires_at: None,
            hostname: "pond-test".into(),
            server_version: "test".into(),
            capabilities: vec![],
            rejection_reason: Some("invalid_mac".into()),
            server_proof: None,
        })
    }
}

/// Handshake stub whose `verify_handshake` always accepts — the success path.
/// (`MockHandshake`'s trait-default `verify_handshake` errors, which is the
/// failure path.)
struct AcceptingHandshake;

#[async_trait]
impl Handshake for AcceptingHandshake {
    async fn handshake(&self, _request: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("not used in this test")
    }
    async fn validate_token(&self, _token: &str) -> Result<bool> {
        Ok(false)
    }
    async fn revoke_device(&self, _device_id: &str) -> Result<u64> {
        Ok(0)
    }
    async fn revoke_token(&self, _token: &str) -> Result<()> {
        Ok(())
    }
    async fn verify_handshake(&self, _request: VerifyRequest) -> Result<HandshakeResponse> {
        Ok(HandshakeResponse {
            accepted: true,
            session_token: Some("fresh-session-token".into()),
            refresh_token: Some("fresh-refresh-token".into()),
            expires_at: None,
            hostname: "pond-test".into(),
            server_version: "test".into(),
            capabilities: vec![],
            rejection_reason: None,
            server_proof: None,
        })
    }
}

struct Harness {
    router: axum::Router,
    notifications: tokio::sync::broadcast::Receiver<Notification>,
    event_log: Arc<dyn EventLog>,
    _tmp: tempfile::TempDir,
}

async fn make_app(handshake: Arc<dyn Handshake>) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();

    let (notification_tx, notifications) = tokio::sync::broadcast::channel::<Notification>(64);
    let queue: Arc<dyn NotificationQueueRepository> =
        Arc::new(SqliteNotificationQueue::new(pool.clone()));
    let sender: Arc<dyn NotificationSender> = Arc::new(BroadcastNotificationSender::new(
        notification_tx.clone(),
        queue.clone(),
        None,
    ));
    let event_log: Arc<dyn EventLog> = Arc::new(SqliteEventLog::new(db.logs.clone()));

    let db = Arc::new(db);
    let state = Arc::new(AppState {
        warmup: Default::default(),
        db,
        onboarding_repo: Arc::new(SqlxOnboardingRepository::new(pool.clone())),
        handshake,
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
        event_bus: None,
        event_log: Some(event_log.clone()),
        push_token_repo: None,
        notification_tx,
        notification_queue: Some(queue),
        notification_sender: Some(sender),
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

    // `/handshake/verify` extracts `ConnectInfo` (per-IP rate limiting); the
    // oneshot test path has no real socket, so inject one.
    let router = build_router(state, std::path::PathBuf::from("pond-desktop/dist")).layer(
        MockConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 40000))),
    );
    Harness {
        router,
        notifications,
        event_log,
        _tmp: tmp,
    }
}

async fn post_verify(router: &axum::Router, device_name: &str) -> StatusCode {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/handshake/verify")
        .header("Content-Type", "application/json")
        .body(Body::from(format!(
            r#"{{"challenge_id":"c-1","mac":"00","device_name":"{device_name}"}}"#
        )))
        .unwrap();
    router.clone().oneshot(req).await.unwrap().status()
}

async fn auth_actions(event_log: &Arc<dyn EventLog>) -> Vec<String> {
    event_log
        .query(EventQuery {
            category: Some(EventCategory::Auth),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.action)
        .collect()
}

#[tokio::test]
async fn successful_pairing_notifies_devices_and_records_an_auth_event() {
    let mut h = make_app(Arc::new(AcceptingHandshake)).await;

    let status = post_verify(&h.router, "Amina's Phone").await;
    assert_eq!(status, StatusCode::OK);

    let n = h
        .notifications
        .try_recv()
        .expect("a notification broadcast");
    assert_eq!(n.category, "info");
    assert_eq!(n.title, "New device paired");
    assert!(n.body.contains("Amina's Phone"), "body: {}", n.body);

    assert_eq!(auth_actions(&h.event_log).await, vec!["auth.device_paired"]);
}

/// Every failure-path assertion lives in this one test on purpose.
///
/// The failure ALERT is debounced by a process-wide `static` with a ten-minute
/// window, so across a test binary only the first failure anywhere can observe a
/// broadcast. Split across two tests, whichever happened to run first would take
/// the alert and the other would fail on ordering alone. Keeping them together
/// makes the order explicit -- and lets the debounce itself be asserted rather
/// than merely worked around.
#[tokio::test]
async fn failed_pairing_alerts_once_records_why_and_debounces() {
    // A real wrong code: `Ok(accepted: false)` with a reason, which is a 200
    // carrying a refusal. This is the path an operator actually hits.
    let mut rejected = make_app(Arc::new(RejectingHandshake)).await;
    assert_eq!(
        post_verify(&rejected.router, "Amina's Phone").await,
        StatusCode::OK,
        "a refusal is a 200 carrying a reason, not an error"
    );

    let n = rejected
        .notifications
        .try_recv()
        .expect("an alert broadcast");
    assert_eq!(n.category, "alert");
    assert_eq!(n.title, "Failed pairing attempt");

    let events = rejected
        .event_log
        .query(EventQuery {
            category: Some(EventCategory::Auth),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, "auth.pairing_verify_failed");
    // The reason is the point. Without it the log records that a pairing failed
    // but not what went wrong, which is the one thing worth going back for.
    assert_eq!(
        events[0].attributes.get("rejection_reason"),
        Some(&AttributeValue::Text("invalid_mac".into())),
    );
    // Sensitive (visible to the audit tools) — never Secret, and carrying no
    // MAC or token material.
    assert_eq!(events[0].privacy_sensitivity, PrivacySensitivity::Sensitive);

    // A handshake that errors outright is the other failure shape: still a
    // recorded auth event, and still no alert, because the window is spent.
    let mut errored = make_app(Arc::new(MockHandshake::new())).await;
    assert_eq!(
        post_verify(&errored.router, "intruder").await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(
        errored.notifications.try_recv().is_err(),
        "a burst of guesses must produce one alert per window, not one per guess"
    );
    let actions = auth_actions(&errored.event_log).await;
    assert_eq!(actions, vec!["auth.pairing_verify_failed"]);
}
