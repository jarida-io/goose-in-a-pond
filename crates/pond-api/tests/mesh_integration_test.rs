//! #132 Milestone 6: the `/api/v1/mesh/peers` and `/api/v1/mesh/self` routes,
//! driven through a real router with real SQLite `PeerDirectory`/`CreditLedger`
//! in `AppState`. `mesh_transport` stays `None`, as in every other test AppState:
//! trust-circle CRUD needs no live networking.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::mesh::domain::capabilities::PeerCapabilities;
use pond_core::mesh::domain::millisats::Millisats;
use pond_core::mesh::domain::peer_id::PeerId;
use pond_core::mesh::domain::settlement::MESH_SETTLEMENT_MILLISATS_PER_TOKEN;
use pond_core::mesh::domain::token_count::TokenCount;
use pond_core::mesh::mocks::mock_peer_capability_query::MockPeerCapabilityQuery;
use pond_core::mesh::ports::credit_ledger::CreditLedger;
use pond_core::mesh::ports::peer_capability_query::PeerCapabilityQuery;
use pond_core::mesh::ports::usage_tally::UsageTally;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::mocks::mock_device_registry::MockDeviceRegistry;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::onboarding::SqlxOnboardingRepository;
use pond_infra::sqlite_credit_ledger::SqliteCreditLedger;
use pond_infra::sqlite_peer_directory::SqlitePeerDirectory;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_usage_tally::SqliteUsageTally;
use tower::ServiceExt;

async fn make_app() -> (axum::Router, Arc<SqliteCreditLedger>, tempfile::TempDir) {
    make_app_with_mesh_provider(None).await
}

async fn make_app_with_mesh_provider(
    mesh_provider: Option<Arc<dyn LlmProvider>>,
) -> (axum::Router, Arc<SqliteCreditLedger>, tempfile::TempDir) {
    make_app_with_mesh_provider_and_capabilities(mesh_provider, None).await
}

async fn make_app_with_mesh_provider_and_capabilities(
    mesh_provider: Option<Arc<dyn LlmProvider>>,
    peer_capability_query: Option<Arc<dyn PeerCapabilityQuery>>,
) -> (axum::Router, Arc<SqliteCreditLedger>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();

    let credit_ledger = Arc::new(SqliteCreditLedger::new(pool.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        suggestion_queue: std::sync::Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db: Arc::new(db),
        onboarding_repo: Arc::new(SqlxOnboardingRepository::new(pool.clone())),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage: Arc::new(SqliteSessionStorage::new(pool.clone())),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        // In a real server, selecting chat_provider="mesh" writes the *same*
        // provider into llm_provider. Mirror that rather than leaving it empty:
        // GET /api/v1/test reads llm_provider directly, never mesh_provider.
        llm_provider: Arc::new(tokio::sync::RwLock::new(mesh_provider.clone())),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(MockDeviceRegistry),
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
        prompt_template_repo: None,
        prompt_extra_repo: None,
        skill_repo: None,
        recipe_repo: None,
        llamafile_manager: None,
        operational_log: None,
        matter: None,
        oauth_outcomes: pond_api::oauth_callback::new_oauth_outcomes(),
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
        security_policy: None,
        tool_dispatcher: None,
        api_port: 4000,
        weather_provider: None,
        peer_directory: Arc::new(SqlitePeerDirectory::new(pool.clone())),
        credit_ledger: credit_ledger.clone(),
        usage_tally: Arc::new(SqliteUsageTally::new(pool.clone())),
        mesh_transport: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_provider: Arc::new(tokio::sync::RwLock::new(mesh_provider)),
        peer_capability_query: Arc::new(tokio::sync::RwLock::new(peer_capability_query)),
        mesh_rebuild: None,
    });

    (
        build_router(state, std::path::PathBuf::from("web/dist")),
        credit_ledger,
        tmp,
    )
}

/// For `/mesh/settlement` tests — needs a real `usage_tally` (to seed
/// pending usage) and a real `settings_repo` (to set the exchange rate),
/// neither of which the other helpers above hand back.
async fn make_app_with_settlement_deps() -> (
    axum::Router,
    Arc<SqliteUsageTally>,
    Arc<MockSettingsRepository>,
    Arc<SqlitePeerDirectory>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();

    let credit_ledger = Arc::new(SqliteCreditLedger::new(pool.clone()));
    let usage_tally = Arc::new(SqliteUsageTally::new(pool.clone()));
    let settings_repo = Arc::new(MockSettingsRepository::new());
    let peer_directory = Arc::new(SqlitePeerDirectory::new(pool.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        account_sync: None,
        suggestion_queue: std::sync::Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db: Arc::new(db),
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
        settings_repo: settings_repo.clone(),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(MockDeviceRegistry),
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
        lane: None,
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
        matter: None,
        oauth_outcomes: pond_api::oauth_callback::new_oauth_outcomes(),
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
        security_policy: None,
        tool_dispatcher: None,
        api_port: 4000,
        weather_provider: None,
        peer_directory: peer_directory.clone(),
        credit_ledger,
        usage_tally: usage_tally.clone(),
        mesh_transport: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_provider: Arc::new(tokio::sync::RwLock::new(None)),
        peer_capability_query: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_rebuild: None,
    });

    (
        build_router(state, std::path::PathBuf::from("web/dist")),
        usage_tally,
        settings_repo,
        peer_directory,
        tmp,
    )
}

async fn json_request(
    app: &axum::Router,
    method: Method,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(match body {
            Some(b) => Body::from(b.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn self_reports_disabled_when_mesh_transport_absent() {
    let (app, _ledger, _tmp) = make_app().await;
    let (status, body) = json_request(&app, Method::GET, "/api/v1/mesh/self", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mesh_enabled"], false);
}

#[tokio::test]
async fn peers_list_starts_empty() {
    let (app, _ledger, _tmp) = make_app().await;
    let (status, body) = json_request(&app, Method::GET, "/api/v1/mesh/peers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["peers"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn add_list_remove_peer_roundtrips() {
    let (app, ledger, _tmp) = make_app().await;
    let peer = PeerId::from([7u8; 32]);
    ledger.credit(peer, Millisats::new(500)).await.unwrap();

    let (status, body) = json_request(
        &app,
        Method::POST,
        "/api/v1/mesh/peers",
        Some(serde_json::json!({
            "peer_id": peer.to_string(),
            "trust_scope": "circle",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["peer_id"], peer.to_string());
    assert_eq!(body["trust_scope"], "circle");

    let (status, body) = json_request(&app, Method::GET, "/api/v1/mesh/peers", None).await;
    assert_eq!(status, StatusCode::OK);
    let peers = body["peers"].as_array().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0]["peer_id"], peer.to_string());
    assert_eq!(peers[0]["trust_scope"], "circle");
    assert_eq!(peers[0]["connected"], false);
    assert_eq!(peers[0]["credit_balance_millisats"], 500);

    let (status, _) = json_request(
        &app,
        Method::DELETE,
        &format!("/api/v1/mesh/peers/{peer}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = json_request(&app, Method::GET, "/api/v1/mesh/peers", None).await;
    assert_eq!(body["peers"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn add_peer_rejects_unknown_trust_scope() {
    let (app, _ledger, _tmp) = make_app().await;
    let peer = PeerId::from([8u8; 32]);
    let (status, _) = json_request(
        &app,
        Method::POST,
        "/api/v1/mesh/peers",
        Some(serde_json::json!({
            "peer_id": peer.to_string(),
            "trust_scope": "not_a_real_scope",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn add_peer_rejects_malformed_peer_id() {
    let (app, _ledger, _tmp) = make_app().await;
    let (status, _) = json_request(
        &app,
        Method::POST,
        "/api/v1/mesh/peers",
        Some(serde_json::json!({
            "peer_id": "not-hex",
            "trust_scope": "circle",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ── mesh_provider wiring (#132 Milestone 3.5) ─────────────────────────────
// App-level wiring only; pond-adapters-mesh-inference tests its own round-trip.
// With chat_provider="mesh" resolved into both mesh_provider and llm_provider,
// GET /api/v1/test must genuinely round-trip rather than silently report nothing.

#[tokio::test]
async fn test_endpoint_reports_ok_through_the_wired_mesh_provider() {
    let mesh_provider: Arc<dyn LlmProvider> =
        Arc::new(pond_core::models::mocks::mock_provider::MockProvider::new());
    let (app, _ledger, _tmp) = make_app_with_mesh_provider(Some(mesh_provider)).await;

    let (status, body) = json_request(&app, Method::GET, "/api/v1/test", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["llm"]["status"], "ok");
    assert_eq!(body["llm"]["provider"], "mock-v1");
    // test_services always sends ChatMessage::user("pong") — MockProvider's
    // canned reply for that is deterministic, so this proves the request
    // really went through the wired provider, not a stub.
    assert_eq!(body["llm"]["response"], "Mock response to: pong");
}

#[tokio::test]
async fn test_endpoint_reports_not_configured_when_mesh_provider_is_absent() {
    let (app, _ledger, _tmp) = make_app().await;
    let (status, body) = json_request(&app, Method::GET, "/api/v1/test", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["llm"]["status"], "not_configured");
}

// ── Credit top-up (#132) ───────────────────────────────────────────────────

#[tokio::test]
async fn crediting_a_trusted_peer_increases_the_balance_it_reports() {
    let (app, _ledger, _tmp) = make_app().await;
    let peer = PeerId::from([9u8; 32]);
    json_request(
        &app,
        Method::POST,
        "/api/v1/mesh/peers",
        Some(serde_json::json!({
            "peer_id": peer.to_string(),
            "trust_scope": "circle",
        })),
    )
    .await;

    let (status, body) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/mesh/peers/{peer}/credit"),
        Some(serde_json::json!({ "amount_millisats": 500 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["peer_id"], peer.to_string());
    assert_eq!(body["credit_balance_millisats"], 500);

    // A second top-up accumulates rather than overwriting.
    let (_, body) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/mesh/peers/{peer}/credit"),
        Some(serde_json::json!({ "amount_millisats": 250 })),
    )
    .await;
    assert_eq!(body["credit_balance_millisats"], 750);

    let (_, body) = json_request(&app, Method::GET, "/api/v1/mesh/peers", None).await;
    assert_eq!(body["peers"][0]["credit_balance_millisats"], 750);
}

#[tokio::test]
async fn crediting_an_untrusted_peer_is_rejected() {
    let (app, _ledger, _tmp) = make_app().await;
    let peer = PeerId::from([10u8; 32]);
    let (status, _) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/mesh/peers/{peer}/credit"),
        Some(serde_json::json!({ "amount_millisats": 500 })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn crediting_zero_is_rejected() {
    let (app, _ledger, _tmp) = make_app().await;
    let peer = PeerId::from([11u8; 32]);
    json_request(
        &app,
        Method::POST,
        "/api/v1/mesh/peers",
        Some(serde_json::json!({
            "peer_id": peer.to_string(),
            "trust_scope": "circle",
        })),
    )
    .await;

    let (status, _) = json_request(
        &app,
        Method::POST,
        &format!("/api/v1/mesh/peers/{peer}/credit"),
        Some(serde_json::json!({ "amount_millisats": 0 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn capabilities_of_a_trusted_peer_are_returned() {
    let query = Arc::new(MockPeerCapabilityQuery::new());
    let peer = PeerId::from([12u8; 32]);
    query
        .set(
            peer,
            PeerCapabilities {
                inference_available: true,
                lightning_available: false,
            },
        )
        .await;
    let (app, _ledger, _tmp) = make_app_with_mesh_provider_and_capabilities(
        None,
        Some(query as Arc<dyn PeerCapabilityQuery>),
    )
    .await;

    json_request(
        &app,
        Method::POST,
        "/api/v1/mesh/peers",
        Some(serde_json::json!({
            "peer_id": peer.to_string(),
            "trust_scope": "circle",
        })),
    )
    .await;

    let (status, body) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/mesh/peers/{peer}/capabilities"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["inference_available"], true);
    assert_eq!(body["lightning_available"], false);
}

#[tokio::test]
async fn capabilities_of_an_untrusted_peer_is_rejected() {
    let query = Arc::new(MockPeerCapabilityQuery::new());
    let (app, _ledger, _tmp) = make_app_with_mesh_provider_and_capabilities(
        None,
        Some(query as Arc<dyn PeerCapabilityQuery>),
    )
    .await;
    let peer = PeerId::from([13u8; 32]);

    let (status, _) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/mesh/peers/{peer}/capabilities"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn capabilities_route_is_unavailable_without_mesh_configured() {
    let (app, _ledger, _tmp) = make_app().await;
    let peer = PeerId::from([14u8; 32]);
    json_request(
        &app,
        Method::POST,
        "/api/v1/mesh/peers",
        Some(serde_json::json!({
            "peer_id": peer.to_string(),
            "trust_scope": "circle",
        })),
    )
    .await;

    let (status, _) = json_request(
        &app,
        Method::GET,
        &format!("/api/v1/mesh/peers/{peer}/capabilities"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

// ── GET /api/v1/mesh/settlement ──────────────────────────────────────────────

#[tokio::test]
async fn settlement_status_reports_the_fixed_rate() {
    let (app, _usage_tally, _settings_repo, _peer_directory, _tmp) =
        make_app_with_settlement_deps().await;

    let (status, body) = json_request(&app, Method::GET, "/api/v1/mesh/settlement", None).await;
    assert_eq!(status, StatusCode::OK);
    // The rate is a fixed constant now, so there is no "unconfigured" state left
    // to report: `configured` is true on a pond that has never touched mesh
    // settlement, because the rate it would settle at is already decided.
    assert_eq!(body["configured"], true);
    assert_eq!(
        body["millisats_per_token"],
        MESH_SETTLEMENT_MILLISATS_PER_TOKEN
    );
    assert_eq!(body["peers"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn settlement_status_reports_pending_usage_per_peer() {
    let (app, usage_tally, _settings_repo, _peer_directory, _tmp) =
        make_app_with_settlement_deps().await;
    let peer = PeerId::from([9u8; 32]);

    json_request(
        &app,
        Method::POST,
        "/api/v1/mesh/peers",
        Some(serde_json::json!({
            "peer_id": peer.to_string(),
            "trust_scope": "circle",
        })),
    )
    .await;
    usage_tally
        .record_borrowed(peer, TokenCount::new(250))
        .await
        .unwrap();

    let (status, body) = json_request(&app, Method::GET, "/api/v1/mesh/settlement", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], true);
    let peers = body["peers"].as_array().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0]["peer_id"], peer.to_string());
    assert_eq!(peers[0]["pending_tokens"], 250);
    // 250 borrowed tokens, priced at the fixed rate.
    assert_eq!(
        peers[0]["pending_millisats"],
        250 * MESH_SETTLEMENT_MILLISATS_PER_TOKEN
    );
}

/// The rate is no longer a per-Pond setting, though the `Settings` field outlived
/// the change. Writing it must move nothing: if the handler is ever re-wired to
/// read settings again this fails here, rather than a household quietly settling
/// at a rate the mesh does not honour.
#[tokio::test]
async fn the_legacy_per_pond_setting_no_longer_moves_the_rate() {
    let (app, usage_tally, settings_repo, _peer_directory, _tmp) =
        make_app_with_settlement_deps().await;
    let peer = PeerId::from([10u8; 32]);

    json_request(
        &app,
        Method::POST,
        "/api/v1/mesh/peers",
        Some(serde_json::json!({
            "peer_id": peer.to_string(),
            "trust_scope": "circle",
        })),
    )
    .await;
    usage_tally
        .record_borrowed(peer, TokenCount::new(100))
        .await
        .unwrap();

    let mut settings = settings_repo.get().await.unwrap();
    settings.mesh_settlement_millisats_per_token = 5;
    settings_repo.update(&settings).await.unwrap();

    let (status, body) = json_request(&app, Method::GET, "/api/v1/mesh/settlement", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], true);
    // 5 was written to settings just above and is deliberately NOT what comes
    // back: the constant wins.
    assert_eq!(
        body["millisats_per_token"],
        MESH_SETTLEMENT_MILLISATS_PER_TOKEN
    );
    let peers = body["peers"].as_array().unwrap();
    assert_eq!(peers[0]["pending_tokens"], 100);
    assert_eq!(
        peers[0]["pending_millisats"],
        100 * MESH_SETTLEMENT_MILLISATS_PER_TOKEN
    );
}
