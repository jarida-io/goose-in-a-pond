//! A minimal real [`AppState`] for integration tests in this crate and in `pond-server`.
//!
//! Pairing, the device registry, profiles, sessions and onboarding use the real SQLite
//! adapters, because the security boundaries under test live in them. Everything else is
//! a mock or absent. Override a field with struct update syntax before wrapping in `Arc`.

use std::sync::Arc;

use pond_core::mcp::ports::notification::Notification;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_infra::db::Database;
use pond_infra::sqlite_device_registry::SqliteDeviceRegistry;
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;

use crate::AppState;

/// The TLS key pin of the Pond [`app_state`] builds, as `tls_identity::pin()` spells one.
pub const TEST_SPKI_PIN: &str = "sha256/TnkUsN+AaLec3BDTh/KUwSokXTmCq2+4rlfBnmJxPkE=";

/// An [`AppState`] over a fresh database in its own temporary data directory.
pub struct TestState {
    /// Not yet shared, so a test can replace fields before wrapping it.
    pub state: AppState,
    /// The real pairing adapter inside `state`, for minting codes and challenges.
    pub handshake: Arc<SqliteHandshakeAdapter>,
    /// The data directory; dropping it deletes both databases.
    pub dir: tempfile::TempDir,
}

/// Apply every migration to an empty data directory and build the state over it.
pub async fn app_state() -> TestState {
    let dir = tempfile::tempdir().expect("temporary data directory");
    let db = Database::init(dir.path()).await.expect("migrations apply");
    let pool = db.system.clone();
    let db = Arc::new(db);
    let profiles = Arc::new(SqliteProfileRepository::new(pool.clone()));
    let handshake = Arc::new(SqliteHandshakeAdapter::new(
        pool.clone(),
        Some(TEST_SPKI_PIN.to_string()),
    ));
    let state = AppState {
        warmup: Default::default(),
        db,
        onboarding_repo: Arc::new(pond_infra::onboarding::SqlxOnboardingRepository::new(
            pool.clone(),
        )),
        handshake: handshake.clone(),
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage: Arc::new(SqliteSessionStorage::new(pool.clone())),
        http_client: pond_net::client(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: profiles.clone(),
        device_registry: Arc::new(SqliteDeviceRegistry::new(pool.clone())),
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
        data_dir: Some(dir.path().to_path_buf()),
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
        notification_tx: tokio::sync::broadcast::channel::<Notification>(16).0,
        notification_queue: None,
        notification_sender: None,
        runs: Arc::new(crate::runs::RunSupervisor::default()),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: None,
        lane: None,
        extraction_status: None,
        suggestion_queue: Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(pool.clone()),
        ),
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
        oauth_state: crate::oauth_callback::new_oauth_state(),
        oauth_outcomes: crate::oauth_callback::new_oauth_outcomes(),
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
        setup_claims: None,
        spoken_codes: Default::default(),
        voice_samples: Default::default(),
    };
    TestState {
        state,
        handshake,
        dir,
    }
}

/// Pair `client_id` through `router` as a phone does: CPace on `/handshake/pake/{init,verify}`
/// with `code`, under the key pin `pin`. Returns the status and body of the last step reached;
/// an accepted pair has already had its `server_proof` checked, as the app checks it.
pub async fn pair_phone(
    router: &axum::Router,
    code: &str,
    client_id: &str,
    pin: &str,
) -> (axum::http::StatusCode, serde_json::Value) {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use pond_infra::pairing_cpace::{channel_identifier, session_id, Responder};

    let nonce = [0x5au8; 32];
    let (status, challenge) = post_json(
        router,
        "/api/v1/handshake/pake/init",
        serde_json::json!({
            "client_id": client_id, "client_type": "gotg", "client_version": "test",
            "channel_binding": pin, "client_nonce": B64.encode(nonce),
        }),
    )
    .await;
    if status != axum::http::StatusCode::OK {
        return (status, challenge);
    }
    let field = |name: &str| {
        B64.decode(
            challenge[name]
                .as_str()
                .expect("the challenge carries this field"),
        )
        .expect("standard base64")
    };
    let phone = Responder::respond(
        code.as_bytes(),
        &channel_identifier(pin, client_id),
        &session_id(&field("challenge"), &nonce),
        &field("share"),
    )
    .expect("the Pond's share is a valid group element");
    let confirm: String = phone
        .confirmation()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let (status, body) = post_json(
        router,
        "/api/v1/handshake/pake/verify",
        serde_json::json!({
            "challenge_id": challenge["challenge_id"], "device_name": client_id,
            "share": B64.encode(phone.share()), "confirm": confirm,
        }),
    )
    .await;
    if body["accepted"] == true {
        let proof = body["server_proof"]
            .as_str()
            .expect("an accepted pair carries a proof");
        let proof: Vec<u8> = (0..proof.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&proof[i..i + 2], 16).expect("hex"))
            .collect();
        assert!(
            phone.verify_server(&proof),
            "the Pond's server_proof did not verify"
        );
    }
    (status, body)
}

async fn post_json(
    router: &axum::Router,
    path: &str,
    body: serde_json::Value,
) -> (axum::http::StatusCode, serde_json::Value) {
    use tower::ServiceExt as _;
    let response = router
        .clone()
        .oneshot(
            axum::http::Request::post(path)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .expect("a valid request"),
        )
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
        .await
        .expect("a readable body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}
