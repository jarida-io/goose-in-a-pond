//! HTTP-level coverage for `POST /api/v1/factory-reset`: owner-only, refused on a name mismatch
//! or a held mesh-credit balance, and otherwise erases the household's SQLite rows and reopens
//! onboarding. `factory_reset.rs` (pond-core) covers the service's own sequencing; this covers
//! the route wiring — permission check, request parsing, `AppState` assembly, and status codes.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::mesh::domain::millisats::Millisats;
use pond_core::mesh::domain::peer_id::PeerId;
use pond_core::mesh::mocks::mock_credit_ledger::MockCreditLedger;
use pond_core::mesh::ports::credit_ledger::CreditLedger as _;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::mocks::mock_device_registry::MockDeviceRegistry;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::onboarding::SqlxOnboardingRepository;
use pond_infra::personal_data_wipe::SystemDataWipe;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_settings::SqliteSettingsRepository;
use tower::ServiceExt;

const POND_NAME: &str = "The Kitchen Pond";

async fn make_app() -> (axum::Router, Arc<Database>, Arc<MockCreditLedger>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::init(tmp.path()).await.unwrap());
    let pool = db.system.clone();

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let settings_repo = Arc::new(SqliteSettingsRepository::new(pool.clone()));
    let mut settings = settings_repo.get().await.unwrap();
    settings.home_name = POND_NAME.to_string();
    settings_repo.update(&settings).await.unwrap();

    let session_storage = Arc::new(SqliteSessionStorage::new(pool.clone()));
    let credit_ledger = Arc::new(MockCreditLedger::new());

    let state = Arc::new(AppState {
        data_wipe: Some(Arc::new(SystemDataWipe::new(db.clone(), None, None))),
        warmup: Default::default(),
        pond_admin: None,
        suggestion_queue: Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db: db.clone(),
        onboarding_repo: Arc::new(SqlxOnboardingRepository::new(pool.clone())),
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
        settings_repo: settings_repo.clone(),
        profile_repo: Arc::new(SqliteProfileRepository::new(pool.clone())),
        device_registry: Arc::new(MockDeviceRegistry),
        room_registry: Arc::new(pond_core::user_data::mocks::mock_room_registry::MockRoomRegistry),
        guest_room_access: Arc::new(pond_core::user_data::mocks::mock_guest_room_access::MockGuestRoomAccess),
        guest_device_access: Arc::new(pond_infra::sqlite_guest_device_access::SqliteGuestDeviceAccess::new(pool.clone())),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedder: Default::default(),
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
        engine_availability: pond_core::models::domain::engine::EngineAvailability {
            litert_library: true,
        },
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
        credit_ledger: credit_ledger.clone(),
        usage_tally: Arc::new(pond_core::mesh::mocks::mock_usage_tally::MockUsageTally::new()),
        mesh_transport: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_provider: Arc::new(tokio::sync::RwLock::new(None)),
        peer_capability_query: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_rebuild: None,
        setup_claims: None,
        spoken_codes: Default::default(),
        voice_samples: Default::default(),
    });

    (
        build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        db,
        credit_ledger,
        tmp,
    )
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
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn the_owner_typing_the_real_name_wipes_the_household_and_reopens_onboarding() {
    let (router, db, _ledger, _tmp) = make_app().await;

    let profile_repo = SqliteProfileRepository::new(db.system.clone());
    profile_repo
        .create(CreateProfileRequest {
            display_name: "Ada".to_string(),
            avatar_emoji: "\u{1F469}".to_string(),
        })
        .await
        .unwrap();

    let resp = router
        .clone()
        .oneshot(post(
            "/api/v1/factory-reset",
            serde_json::json!({"pond_name": POND_NAME}),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["routines"].as_array().unwrap().len(), 0);

    let profiles: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM profiles")
        .fetch_one(&db.system)
        .await
        .unwrap();
    assert_eq!(profiles.0, 0, "the household's profiles must be gone");
}

#[tokio::test]
async fn an_unnamed_pond_refuses_rather_than_let_an_empty_typed_name_match() {
    let (router, db, _ledger, _tmp) = make_app().await;
    let profile_repo = SqliteProfileRepository::new(db.system.clone());
    profile_repo
        .create(CreateProfileRequest {
            display_name: "Ada".to_string(),
            avatar_emoji: "\u{1F469}".to_string(),
        })
        .await
        .unwrap();

    // `make_app` names the Pond; undo that so this Pond has no name, same as a fresh install
    // that never set one.
    sqlx::query("DELETE FROM settings WHERE key = 'home_name'")
        .execute(&db.system)
        .await
        .unwrap();

    let resp = router
        .clone()
        .oneshot(post(
            "/api/v1/factory-reset",
            serde_json::json!({"pond_name": ""}),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["error"], "pond_has_no_name");

    let profiles: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM profiles")
        .fetch_one(&db.system)
        .await
        .unwrap();
    assert_eq!(profiles.0, 1, "a refused reset must not touch the household");
}

#[tokio::test]
async fn a_mistyped_name_refuses_with_409_and_touches_nothing() {
    let (router, db, _ledger, _tmp) = make_app().await;

    let profile_repo = SqliteProfileRepository::new(db.system.clone());
    profile_repo
        .create(CreateProfileRequest {
            display_name: "Ada".to_string(),
            avatar_emoji: "\u{1F469}".to_string(),
        })
        .await
        .unwrap();

    let resp = router
        .clone()
        .oneshot(post(
            "/api/v1/factory-reset",
            serde_json::json!({"pond_name": "Not The Right Name"}),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["error"], "pond_name_mismatch");

    let profiles: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM profiles")
        .fetch_one(&db.system)
        .await
        .unwrap();
    assert_eq!(profiles.0, 1, "a refused reset must not touch the household");
}

#[tokio::test]
async fn a_held_mesh_credit_balance_refuses_with_409() {
    let (router, db, ledger, _tmp) = make_app().await;
    let profile_repo = SqliteProfileRepository::new(db.system.clone());
    profile_repo
        .create(CreateProfileRequest {
            display_name: "Ada".to_string(),
            avatar_emoji: "\u{1F469}".to_string(),
        })
        .await
        .unwrap();

    ledger
        .credit(PeerId::from([9u8; 32]), Millisats::new(500))
        .await
        .unwrap();

    let resp = router
        .clone()
        .oneshot(post(
            "/api/v1/factory-reset",
            serde_json::json!({"pond_name": POND_NAME}),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["error"], "payments_held");

    let profiles: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM profiles")
        .fetch_one(&db.system)
        .await
        .unwrap();
    assert_eq!(profiles.0, 1, "a refused reset must not touch the household");
}
