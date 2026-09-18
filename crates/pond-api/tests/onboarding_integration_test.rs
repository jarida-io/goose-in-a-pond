//! Integration tests — verifies protected routes are blocked before onboarding
//!
//! Run: cargo test -p pond-api --test onboarding_integration_test

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;

use pond_api::{build_router, AppState};
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

    // Required, not defaulted (PAI-2 P7): a stub that inherited "not onboarded" would make
    // every onboarding write route public in whatever test used it. This one tracks the same
    // cell the rest of the mock does, which is what makes
    // `reset_then_recover_is_not_a_one_way_door` a real round trip.
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

/// A repository that cannot read its own table. `get_current_step` has no way to say so: it
/// returns `None`, which every consumer reads as "not started", the state in which every
/// onboarding write route is public. This stub is the fixture for the branch of
/// `pond_is_onboarded` that refuses to inherit that answer.
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
        // The real repository, not the mock: `MockSettingsRepository` stores a hand-written
        // subset and silently drops `chat_model`, while `complete_onboarding` refuses with
        // 400 when `chat_model` is empty. A wizard round trip against the mock can never
        // finish, so it is a fixture production cannot produce.
        settings_repo: Arc::new(pond_infra::sqlite_settings::SqliteSettingsRepository::new(
            settings_pool,
        )),
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
        skip_onboarding: false,
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

#[tokio::test]
async fn settings_is_blocked_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/settings")
                // The token is what makes this measure the ONBOARDING gate. Auth runs
                // first, so without it the answer is 401 and the test says nothing
                // about onboarding at all.
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

/// PAI-2 P0, over real HTTP rather than against `is_public_route` directly. `GET /settings`
/// serialises the whole `Settings` struct, API keys included, so it must be refused without a
/// token. The unit guards in `middleware` prove the table is right; this proves the refusal
/// survives the router, the middleware stack and the onboarding guard in their real order.
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
    // Onboarding is complete, so 403 cannot be masking this: the only thing
    // left to refuse the request is auth.
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// Enough of a `Settings` patch that `POST /onboard/complete` accepts it --
/// the handler refuses to lift the onboarding guard on a half-set-up pond.
const FULL_SETUP: &str =
    r#"{"user_name":"Jerry","assistant_name":"Goose","timezone":"UTC","chat_model":"mock"}"#;

async fn put_settings_no_token(app: &axum::Router, body: &'static str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/settings")
                .header("Content-Type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// Anonymous `POST /onboard/reset` from a named peer, which is what production
/// supplies via `into_make_service_with_connect_info`.
async fn reset_no_token(app: &axum::Router, peer: SocketAddr) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/onboard/reset")
                .extension(ConnectInfo(peer))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

const AT_THE_POND: SocketAddr = SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
    51234,
);
const ON_THE_LAN: SocketAddr = SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 50)),
    44321,
);

/// The other half of P0's fix: the write onboarding depends on stays open while onboarding
/// is running. If this ever returns 401 the wizard cannot save anything before a device has
/// paired, and onboarding deadlocks on a pond nobody can finish setting up. The fixture must
/// be a pond mid-wizard: the same assertion on a COMPLETED pond is the hole PAI-2 P7 closes.
#[tokio::test]
async fn put_settings_without_a_token_is_allowed_while_the_wizard_is_running() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Basics)).await;
    assert_ne!(
        put_settings_no_token(&app, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED,
        "PUT /settings must stay reachable without a token mid-onboarding, or the \
         wizard cannot save"
    );
}

/// PAI-2 P7: and it closes the moment the pond is set up.
///
/// Before this, an unauthenticated caller anywhere on the LAN could rewrite the
/// assistant's settings on a pond that finished onboarding months ago.
#[tokio::test]
async fn put_settings_without_a_token_is_refused_once_onboarded() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    assert_eq!(
        put_settings_no_token(&app, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED
    );
}

/// PAI-2 P7's real acceptance test: the closure must not be a latch. `POST /onboard/reset`
/// stays reachable as the recovery lever, and getting back out of a reset needs
/// `PUT /settings`, `POST /profiles` and `POST /onboard/complete`, all closed on an onboarded
/// pond. It runs on ONE router with no restart, so the gate has to read the state live.
#[tokio::test]
async fn reset_then_recover_is_not_a_one_way_door() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;

    // 1. Set up: the wizard's writes do not answer an anonymous caller.
    assert_eq!(
        put_settings_no_token(&app, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED
    );

    // 2. The operator at the pond resets it -- no token, loopback peer. The
    //    same trust boundary `handshake_issue_pairing_code` already uses to
    //    decide who may pair a new device.
    assert_eq!(
        reset_no_token(&app, AT_THE_POND).await,
        StatusCode::OK,
        "reset must stay reachable from the host, or a pond misconfigured badly \
         enough to lose every token can only be fixed by reflashing it"
    );

    // 3. The door reopened, same router, no restart. A latch fails here.
    assert_eq!(
        put_settings_no_token(&app, FULL_SETUP).await,
        StatusCode::OK,
        "after a reset the wizard must be able to save again"
    );

    let res: Response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/profiles")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"display_name":"Jerry"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::CREATED,
        "POST /profiles must reopen after a reset -- the wizard creates the first \
         household member before anything can pair"
    );

    // 4. ...and finish, which is what re-arms the closure.
    let res: Response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/onboard/complete")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 5. Closed again. A gate that only ever opens is the same defect wearing
    //    the other face.
    assert_eq!(
        put_settings_no_token(&app, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED,
        "the closure must re-arm when onboarding completes again"
    );
}

/// On failure, access NARROWS, asserted through the middleware rather than the adapter.
/// `SqlxOnboardingRepository::get_current_step` ends `.ok()??`, so a database error reads as
/// "not started" -- the state in which the wizard's writes answer anonymous callers. A
/// transient `SQLITE_BUSY` must not reopen every hole this phase closes.
#[tokio::test]
async fn an_unreadable_onboarding_table_closes_the_write_holes_rather_than_opening_them() {
    let (app, _tmp) = app_with_repo(Arc::new(UnreadableRepo)).await;
    assert_eq!(
        put_settings_no_token(&app, FULL_SETUP).await,
        StatusCode::UNAUTHORIZED,
        "a pond whose onboarding state cannot be read must refuse the wizard's \
         writes, not offer them to anyone who asks"
    );
}

/// The assertion that makes the rest of P7 mean anything. Reset re-opens every onboarding
/// write hole by design, so if it answered an anonymous caller on the LAN the closure would
/// be decorative: reset, then walk in through the holes the reset reopened.
#[tokio::test]
async fn reset_from_the_lan_without_a_token_is_refused_once_onboarded() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    assert_eq!(
        reset_no_token(&app, ON_THE_LAN).await,
        StatusCode::UNAUTHORIZED,
        "an unauthenticated phone on the LAN must not be able to factory-reset the pond"
    );
}

/// And the dashboard's own "Start over" control keeps working from anywhere,
/// because it holds a token. `handleRestartOnboarding` calls `resetOnboarding()`
/// while the session token is still set -- sign-out is a separate button -- so
/// this is the path the shipped UI actually takes.
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
