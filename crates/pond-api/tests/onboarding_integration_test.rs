//! Route access before, during and after onboarding.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;

use pond_api::{build_router, AppState};
use pond_core::network::mocks::mock_setup_code_store::MockSetupCodeStore;
use pond_core::network::services::setup_claim::SetupClaimService;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_infra::mock_handshake::MockHandshake;
use reqwest::Client as ReqwestClient;

// ─────────────────────────────────────────────────────────────────
// Minimal mock
// ─────────────────────────────────────────────────────────────────

struct MockRepo {
    step: std::sync::Mutex<Option<OnboardingStep>>,
}

impl MockRepo {
    fn new(step: Option<OnboardingStep>) -> Self {
        Self {
            step: std::sync::Mutex::new(step),
        }
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

    // Tracks the same cell as `get_current_step`, so a reset-and-recover is a real round trip.
    async fn is_complete(&self) -> anyhow::Result<bool> {
        Ok(matches!(
            self.step.lock().ok().and_then(|g| *g),
            Some(OnboardingStep::Completed)
        ))
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
            room_id: req.room_id,
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

/// Cannot read its table: `get_current_step` returns `None`, which reads as "not started".
struct UnreadableRepo;

#[async_trait::async_trait]
impl OnboardingRepository for UnreadableRepo {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        None
    }
    async fn save_step(&self, _: OnboardingStep) -> anyhow::Result<()> {
        Ok(())
    }
    async fn reset(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn is_complete(&self) -> anyhow::Result<bool> {
        Err(anyhow::anyhow!("database is locked"))
    }
}

async fn app_with_step(step: Option<OnboardingStep>) -> (axum::Router, tempfile::TempDir) {
    app_with_repo(Arc::new(MockRepo::new(step))).await
}

async fn app_with_repo(
    onboarding_repo: Arc<dyn OnboardingRepository + Send + Sync>,
) -> (axum::Router, tempfile::TempDir) {
    app_with(onboarding_repo, None, false).await
}

/// A pond mid-wizard whose setup record lives in `store`, as the server wires its claim latch.
async fn claimable_app(
    step: Option<OnboardingStep>,
    store: &Arc<MockSetupCodeStore>,
) -> (axum::Router, tempfile::TempDir) {
    let claims = Arc::new(SetupClaimService::new(store.clone()));
    app_with(Arc::new(MockRepo::new(step)), Some(claims), false).await
}

async fn app_with(
    onboarding_repo: Arc<dyn OnboardingRepository + Send + Sync>,
    setup_claims: Option<Arc<SetupClaimService>>,
    skip_onboarding: bool,
) -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();

    let session_storage: Arc<dyn pond_core::user_data::ports::session_storage::SessionStorage> =
        Arc::new(pond_infra::sqlite_session_storage::SqliteSessionStorage::new(db.system.clone()));
    let settings_pool = db.system.clone();

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        suggestion_queue: std::sync::Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db: Arc::new(db),
        onboarding_repo,
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
        // Real repository: the mock drops `chat_model`, and `complete_onboarding` 400s without it.
        settings_repo: Arc::new(pond_infra::sqlite_settings::SqliteSettingsRepository::new(
            settings_pool,
        )),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(MockDeviceRegistry),
        room_registry: Arc::new(pond_core::user_data::mocks::mock_room_registry::MockRoomRegistry),
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
        skip_onboarding,
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
        credit_ledger: Arc::new(
            pond_core::mesh::mocks::mock_credit_ledger::MockCreditLedger::new(),
        ),
        usage_tally: Arc::new(pond_core::mesh::mocks::mock_usage_tally::MockUsageTally::new()),
        mesh_transport: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_provider: Arc::new(tokio::sync::RwLock::new(None)),
        peer_capability_query: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_rebuild: None,
        setup_claims,
        spoken_codes: Default::default(),
        voice_samples: Default::default(),
        pond_admin: None,
    });
    (
        // As the dashboard listener wires it, so host recovery can present the credential.
        build_router(state, std::path::PathBuf::from("pond-desktop/dist"))
            .layer(axum::Extension(CREDENTIAL.clone())),
        tmp,
    )
}

/// The host credential the server would have written to its data directory.
static CREDENTIAL: std::sync::LazyLock<pond_api::host_guard::HostCredential> =
    std::sync::LazyLock::new(pond_api::host_guard::HostCredential::generate);

// ─────────────────────────────────────────────────────────────────
// Public routes — must always be accessible
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn health_is_accessible_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn onboard_status_is_accessible_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/onboard/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn system_info_is_accessible_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/info")
                .body(Body::empty())
                .unwrap(),
        )
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
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/chat")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn devices_is_blocked_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/devices")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

/// The wizard resumes from what it saved, so a paired device reads settings before onboarding.
#[tokio::test]
async fn a_paired_device_reads_settings_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/settings")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

/// `GET /settings` serialises every setting; checked through the real stack.
#[tokio::test]
async fn get_settings_without_a_token_is_unauthorized() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    let res: Response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Onboarding is complete, so only auth can refuse this.
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// The minimum `Settings` patch `POST /onboard/complete` accepts.
const FULL_SETUP: &str =
    r#"{"user_name":"Jerry","assistant_name":"Goose","timezone":"UTC","chat_model":"mock"}"#;

async fn put_settings_no_token(
    app: &axum::Router,
    peer: SocketAddr,
    body: &'static str,
) -> StatusCode {
    wizard_write(app, "PUT", "/api/v1/settings", peer, None, body).await
}

/// One wizard request from `peer`, with a bearer when given.
async fn wizard_write(
    app: &axum::Router,
    method: &str,
    uri: &str,
    peer: SocketAddr,
    bearer: Option<&str>,
    body: &'static str,
) -> StatusCode {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("Content-Type", "application/json")
        .extension(ConnectInfo(peer));
    if let Some(bearer) = bearer {
        request = request.header("Authorization", format!("Bearer {bearer}"));
    }
    app.clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap()
        .status()
}

/// Anonymous `POST /onboard/reset` from a peer, as `into_make_service_with_connect_info` supplies.
async fn reset_no_token(app: &axum::Router, peer: SocketAddr) -> StatusCode {
    reset(app, peer, None, None).await.0
}

/// `POST /onboard/reset` from a peer, with a bearer and the host credential when given.
async fn reset(
    app: &axum::Router,
    peer: SocketAddr,
    bearer: Option<&str>,
    credential: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/v1/onboard/reset")
        .extension(ConnectInfo(peer));
    if let Some(bearer) = bearer {
        request = request.header("Authorization", format!("Bearer {bearer}"));
    }
    if let Some(credential) = credential {
        request = request.header(pond_api::host_guard::CREDENTIAL_HEADER, credential);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

const AT_THE_POND: SocketAddr = SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
    51234,
);
const ON_THE_LAN: SocketAddr = SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 50)),
    44321,
);

/// The desktop's wizard can save before it pairs; a 401 here would deadlock onboarding.
#[tokio::test]
async fn put_settings_without_a_token_is_allowed_from_the_host_while_the_wizard_is_running() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Basics)).await;
    assert_ne!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED,
        "PUT /settings must stay reachable without a token from the host mid-onboarding, or \
         the wizard cannot save"
    );
}

/// Until P4 any LAN peer could drive a fresh pond's wizard; a phone now claims it first.
#[tokio::test]
async fn a_tokenless_lan_peer_cannot_drive_a_fresh_ponds_wizard() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Basics)).await;
    assert_eq!(
        put_settings_no_token(&app, ON_THE_LAN, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED
    );
    // A paired phone's token is what admits it.
    assert_eq!(
        wizard_write(
            &app,
            "PUT",
            "/api/v1/settings",
            ON_THE_LAN,
            Some("test-token"),
            FULL_SETUP
        )
        .await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn put_settings_without_a_token_is_refused_once_onboarded() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    for peer in [AT_THE_POND, ON_THE_LAN] {
        assert_eq!(
            put_settings_no_token(&app, peer, FULL_SETUP).await,
            StatusCode::UNAUTHORIZED
        );
    }
}

/// Every wizard route, on a pond a device has claimed: the claimant's token, never the host alone.
#[tokio::test]
async fn a_claimed_ponds_wizard_needs_a_paired_devices_token() {
    let store = Arc::new(MockSetupCodeStore::unclaimed());
    store.set_claimed_at(Some(chrono::Utc::now()));
    let (app, _tmp) = claimable_app(Some(OnboardingStep::Basics), &store).await;
    for (method, uri, body) in [
        ("PUT", "/api/v1/settings", FULL_SETUP),
        ("POST", "/api/v1/profiles", r#"{"display_name":"Jerry"}"#),
        ("POST", "/api/v1/onboard/step/Basics", ""),
        ("POST", "/api/v1/onboard/reset", ""),
    ] {
        assert_eq!(
            wizard_write(&app, method, uri, AT_THE_POND, None, body).await,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} answered a tokenless host caller on a claimed pond"
        );
        for peer in [AT_THE_POND, ON_THE_LAN] {
            let status = wizard_write(&app, method, uri, peer, Some("test-token"), body).await;
            assert!(
                status.is_success(),
                "{method} {uri} refused the claimant's token from {peer}: {status}"
            );
        }
    }
    let status = wizard_write(
        &app,
        "POST",
        "/api/v1/onboard/complete",
        AT_THE_POND,
        Some("test-token"),
        "",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the claimant finishes setup as itself"
    );
    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED
    );
}

/// No cache: `setup-code --rearm` reopens the host's tokenless wizard on the very next request.
#[tokio::test]
async fn the_claim_is_read_live_so_a_rearm_reopens_the_hosts_wizard() {
    let store = Arc::new(MockSetupCodeStore::unclaimed());
    let (app, _tmp) = claimable_app(Some(OnboardingStep::Basics), &store).await;
    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::OK
    );
    store.set_claimed_at(Some(chrono::Utc::now()));
    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED,
        "a claim latched by another process must close the host's wizard at once"
    );
    store.set_claimed_at(None);
    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::OK,
        "a re-armed pond must reopen without a restart, or the claim is a one-way door"
    );
}

/// A failed read narrows: the wizard asks for a token rather than answering the host.
#[tokio::test]
async fn an_unreadable_claim_reads_as_claimed() {
    let store = Arc::new(MockSetupCodeStore::unclaimed());
    store.set_failing(true);
    let (app, _tmp) = claimable_app(Some(OnboardingStep::Basics), &store).await;
    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        wizard_write(
            &app,
            "PUT",
            "/api/v1/settings",
            AT_THE_POND,
            Some("test-token"),
            FULL_SETUP
        )
        .await,
        StatusCode::OK
    );
}

/// Nothing has latched on a pond whose card was never made, so the host's wizard stays open.
#[tokio::test]
async fn a_pond_with_no_setup_record_reads_as_unclaimed() {
    let store = Arc::new(MockSetupCodeStore::new());
    let (app, _tmp) = claimable_app(Some(OnboardingStep::Basics), &store).await;
    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::OK
    );
}

/// `--skip-onboarding` still means "no wizard": its writes are closed to the tokenless host.
#[tokio::test]
async fn skip_onboarding_still_reads_as_set_up() {
    let store = Arc::new(MockSetupCodeStore::unclaimed());
    let claims = Arc::new(SetupClaimService::new(store.clone()));
    let (app, _tmp) = app_with(
        Arc::new(MockRepo::new(Some(OnboardingStep::Basics))),
        Some(claims),
        true,
    )
    .await;
    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED
    );
    let (status, body) = reset(&app, AT_THE_POND, None, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "host_credential_required");
}

/// One router, no restart: the gate must read onboarding state live, not latch.
#[tokio::test]
async fn reset_then_recover_is_not_a_one_way_door() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;

    // 1. Set up: the wizard's writes do not answer an anonymous caller.
    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED
    );

    // 2. Reset from loopback with the host credential, no token: the boundary pairing uses.
    assert_eq!(
        reset(&app, AT_THE_POND, None, Some(CREDENTIAL.as_str()))
            .await
            .0,
        StatusCode::OK,
        "reset must stay reachable from the host, or a pond misconfigured badly \
         enough to lose every token can only be fixed by reflashing it"
    );

    // 3. The door reopened to the host, same router, no restart. A latch fails here.
    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::OK,
        "after a reset the wizard must be able to save again"
    );

    assert_eq!(
        wizard_write(
            &app,
            "POST",
            "/api/v1/profiles",
            AT_THE_POND,
            None,
            r#"{"display_name":"Jerry"}"#
        )
        .await,
        StatusCode::CREATED,
        "POST /profiles must reopen after a reset -- the wizard creates the first \
         household member before anything can pair"
    );

    // 4. ...and finish, which is what re-arms the closure.
    assert_eq!(
        wizard_write(
            &app,
            "POST",
            "/api/v1/onboard/complete",
            AT_THE_POND,
            None,
            ""
        )
        .await,
        StatusCode::OK
    );

    // 5. Closed again.
    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED,
        "the closure must re-arm when onboarding completes again"
    );
}

/// The same cycle on a claimed pond: the reset reopens the wizard to the claimant's token, and
/// a host that has lost every token pairs again (pairing answers in every state) or re-arms.
#[tokio::test]
async fn reset_on_a_claimed_pond_is_not_a_one_way_door_either() {
    let store = Arc::new(MockSetupCodeStore::unclaimed());
    store.set_claimed_at(Some(chrono::Utc::now()));
    let repo = Arc::new(MockRepo::new(Some(OnboardingStep::Completed)));
    let claims = Arc::new(SetupClaimService::new(store.clone()));
    let (app, _tmp) = app_with(repo, Some(claims), false).await;

    let (status, _) = reset(&app, AT_THE_POND, None, Some(CREDENTIAL.as_str())).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the host can still reset a claimed pond"
    );

    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED,
        "a claimed pond's wizard is its claimant's, reset or not"
    );
    for peer in [AT_THE_POND, ON_THE_LAN] {
        assert_eq!(
            wizard_write(
                &app,
                "PUT",
                "/api/v1/settings",
                peer,
                Some("test-token"),
                FULL_SETUP
            )
            .await,
            StatusCode::OK,
            "the claimant's token must reopen the wizard after a reset, from {peer}"
        );
    }

    let pairing = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/handshake/pairing-code")
                .header(pond_api::host_guard::CREDENTIAL_HEADER, CREDENTIAL.as_str())
                .extension(ConnectInfo(AT_THE_POND))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        pairing.status(),
        StatusCode::OK,
        "pairing must answer a claimed, reset pond's host, or a pond that lost every token \
         could only be repaired by reflashing it"
    );

    store.set_claimed_at(None);
    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::OK,
        "a re-armed reset pond is a fresh install again"
    );
}

/// A DB error reads as "not started", so a transient `SQLITE_BUSY` must not open the writes.
#[tokio::test]
async fn an_unreadable_onboarding_table_closes_the_write_holes_rather_than_opening_them() {
    let (app, _tmp) = app_with_repo(Arc::new(UnreadableRepo)).await;
    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED,
        "a pond whose onboarding state cannot be read must refuse the wizard's \
         writes, not offer them to anyone who asks"
    );
}

/// Reset reopens every onboarding write, so an anonymous LAN reset would undo the closure.
#[tokio::test]
async fn reset_from_the_lan_without_a_token_is_refused_once_onboarded() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    assert_eq!(
        reset_no_token(&app, ON_THE_LAN).await,
        StatusCode::UNAUTHORIZED,
        "an unauthenticated phone on the LAN must not be able to factory-reset the pond"
    );
}

/// Another OS account, or a page on another local port, reaches loopback too. Without the
/// credential only the Pond's own account can read, it could re-arm the wizard and open its
/// writes to itself.
#[tokio::test]
async fn reset_from_the_host_needs_the_host_credential_once_onboarded() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    let stale = pond_api::host_guard::HostCredential::generate();
    for presented in [None, Some(stale.as_str())] {
        let (status, body) = reset(&app, AT_THE_POND, None, presented).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{presented:?}");
        assert_eq!(body["error"], "host_credential_required");
    }
    // A bearer the server cannot verify is no substitute.
    let (status, _) = reset(&app, AT_THE_POND, Some("not-a-token"), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        put_settings_no_token(&app, AT_THE_POND, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED,
        "a refused reset reopened the wizard"
    );
}

/// The desktop wizard's own "Start over" can run before it pairs; a phone's runs with its token.
#[tokio::test]
async fn reset_stays_open_to_the_host_while_an_unclaimed_wizard_runs() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Basics)).await;
    assert_eq!(reset_no_token(&app, AT_THE_POND).await, StatusCode::OK);
    assert_eq!(
        reset_no_token(&app, ON_THE_LAN).await,
        StatusCode::UNAUTHORIZED
    );
    let (status, _) = reset(&app, ON_THE_LAN, Some("test-token"), None).await;
    assert_eq!(status, StatusCode::OK);
}

/// The shipped "Start over" (`handleRestartOnboarding`) resets while its token is still set.
#[tokio::test]
async fn the_dashboard_can_still_restart_setup_with_its_token() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    let res: Response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/onboard/reset")
                .header("Authorization", "Bearer test-token")
                .extension(ConnectInfo(ON_THE_LAN))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // From the host the middleware skips the token, so the handler checks it itself.
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    let (status, _) = reset(&app, AT_THE_POND, Some("test-token"), None).await;
    assert_eq!(status, StatusCode::OK);
}

// ─────────────────────────────────────────────────────────────────
// Protected routes — must be accessible after onboarding
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn chat_is_accessible_after_onboarding() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    let res: Response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/chat")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn devices_is_accessible_after_onboarding() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    let res: Response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/devices")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn settings_is_accessible_after_onboarding() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    let res: Response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}
