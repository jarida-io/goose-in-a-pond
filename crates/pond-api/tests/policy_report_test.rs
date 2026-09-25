//! `GET /api/v1/security/policy-report` shows what `enforce` would break: in `audit` mode a
//! would-deny still succeeds, so the response alone cannot tell.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::security::ports::event_log::EventLog;
use pond_core::security::ports::policy::SecurityPolicy;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_device_registry::MockDeviceRegistry;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_event_log::SqliteEventLog;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_security_policy::SqliteSecurityPolicy;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_settings::SqliteSettingsRepository;
use tower::ServiceExt;

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
    async fn is_complete(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

/// A router whose policy really records; with `security_policy: None` nothing is audited.
async fn make_app() -> (axum::Router, Arc<SqliteSessionStorage>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let event_log: Arc<dyn EventLog> = Arc::new(SqliteEventLog::new(db.logs.clone()));
    let db = Arc::new(db);

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let session_storage = Arc::new(SqliteSessionStorage::new(pool.clone()));

    let state = Arc::new(AppState {
        warmup: Default::default(),
        db,
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage: session_storage.clone(),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        tts_control: None,
        // Real settings, so `security_policy_mode` is the shipped default.
        settings_repo: Arc::new(SqliteSettingsRepository::new(pool.clone())),
        // Real profiles: `sessions.profile_id` is a foreign key.
        profile_repo: Arc::new(SqliteProfileRepository::new(pool.clone())),
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
        prompt_template_repo: None,
        prompt_extra_repo: None,
        skill_repo: None,
        recipe_repo: None,
        llamafile_manager: None,
        operational_log: None,
        event_bus: None,
        event_log: Some(event_log.clone()),
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
        security_policy: Some(
            Arc::new(SqliteSecurityPolicy::new(event_log)) as Arc<dyn SecurityPolicy>
        ),
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
        session_storage,
        tmp,
    )
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap()
}

fn put(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(Method::PUT)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// Fetch the report, asserting the status first: an error payload reads as all zeros.
async fn report(app: &axum::Router) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(get("/api/v1/security/policy-report?window=day"))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the report must answer with zeros, never an error, on an empty window"
    );
    body_json(resp).await
}

/// `POLICY_COUNTERS` is process-global: every test that causes a policy decision holds this.
static DECISION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn n(v: &serde_json::Value, block: &str, key: &str) -> u64 {
    v[block][key]
        .as_u64()
        .unwrap_or_else(|| panic!("missing {block}.{key} in {v}"))
}

/// The vacuity control is inline so "zeros before, one after" runs serially on one router.
#[tokio::test]
async fn an_unproved_identity_assertion_shows_up_as_a_would_deny_in_both_halves() {
    let _serial = DECISION_LOCK.lock().await;
    let (app, storage, _tmp) = make_app().await;
    storage
        .create_session("sess-p8a".to_string())
        .await
        .unwrap();

    // ── Vacuity control: an unwired route 404s; this one answers zeros. ────
    let before = report(&app).await;
    assert_eq!(before["window"], "day");
    assert_eq!(n(&before, "events", "allow"), 0);
    assert_eq!(n(&before, "events", "would_deny"), 0);
    assert_eq!(n(&before, "events", "deny"), 0);
    assert_eq!(
        before["events"]["truncated"], false,
        "nothing was scanned, so nothing can have been truncated"
    );
    assert_eq!(
        before["events"]["available"], true,
        "an absent event log would make every events assertion below vacuous"
    );
    let process_would_deny_before = n(&before, "process", "would_deny");
    let process_allow_before = n(&before, "process", "allow");

    // ── Take one decision the policy refuses but audit mode lets through. ──
    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/profiles",
            serde_json::json!({ "display_name": "Liz" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let liz = body_json(resp).await["id"].as_str().unwrap().to_string();

    // A Bearer principal has no proved profile, so this is a refusal that proceeds.
    let resp = app
        .clone()
        .oneshot(put(
            "/api/v1/sessions/sess-p8a/user",
            serde_json::json!({ "profile_id": liz }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "audit mode must not block -- if this 403s the fixture is testing enforce"
    );

    // ── Both halves must now see it, and must NOT see it as an allow. ──────
    let after = report(&app).await;
    assert_eq!(
        n(&after, "events", "would_deny"),
        1,
        "the event half lost the would-deny: {after}"
    );
    assert_eq!(
        n(&after, "events", "allow"),
        0,
        "a refusal that proceeded was recorded as an allow -- the exact confusion \
         `verdict` exists to prevent: {after}"
    );
    assert_eq!(n(&after, "events", "deny"), 0, "audit mode denies nothing");
    assert_eq!(
        n(&after, "events", "unclassified"),
        0,
        "an audit row with no readable verdict attribute: {after}"
    );
    assert_eq!(
        n(&after, "process", "would_deny") - process_would_deny_before,
        1,
        "the process half lost the would-deny: {after}"
    );
    assert_eq!(
        n(&after, "process", "allow") - process_allow_before,
        0,
        "the process half counted a refusal as an allow: {after}"
    );
}

/// The halves stay separate: they span different windows, and only the event half is erasable.
#[tokio::test]
async fn clearing_the_activity_log_cannot_zero_the_process_counters() {
    let _serial = DECISION_LOCK.lock().await;
    let (app, storage, _tmp) = make_app().await;
    storage
        .create_session("sess-clear".to_string())
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/profiles",
            serde_json::json!({ "display_name": "Jerry" }),
        ))
        .await
        .unwrap();
    let jerry = body_json(resp).await["id"].as_str().unwrap().to_string();

    app.clone()
        .oneshot(put(
            "/api/v1/sessions/sess-clear/user",
            serde_json::json!({ "profile_id": jerry }),
        ))
        .await
        .unwrap();

    let seen = report(&app).await;
    assert_eq!(n(&seen, "events", "would_deny"), 1);
    let process_after_decision = n(&seen, "process", "would_deny");

    // The user-facing "clear my activity" button.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/api/v1/activity")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let wiped = report(&app).await;
    assert_eq!(
        n(&wiped, "events", "would_deny"),
        0,
        "the event half is erasable -- that is the premise, not a bug"
    );
    assert!(
        n(&wiped, "process", "would_deny") >= process_after_decision,
        "the process half was taken to zero by a button the household can press; \
         the enforce flip would then be decided from erased evidence: {wiped}"
    );
}

#[tokio::test]
async fn an_unrecognised_window_is_rejected_rather_than_quietly_widened() {
    let (app, _storage, _tmp) = make_app().await;
    let resp = app
        .oneshot(get("/api/v1/security/policy-report?window=forever"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Being in `protected_routes` is what gates this; checked with a real request, not source.
#[tokio::test]
async fn the_report_is_not_readable_without_a_token() {
    let (app, _storage, _tmp) = make_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/security/policy-report")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
