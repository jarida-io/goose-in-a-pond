//! Matter enablement over the real HTTP surface: `POST /devices/commission`,
//! `GET /matter/status` and `PUT /settings` driven through the router with a runtime
//! pinned to each state. Every reason commissioning can be unavailable — off, still
//! starting, controller unreachable — must stay distinguishable, not collapse into one 503.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::mocks::mock_device_registry::MockDeviceRegistry;
use pond_core::user_data::mocks::mock_matter_runtime::StubMatterRuntime;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::matter_runtime::MatterRuntimePort;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::onboarding::SqlxOnboardingRepository;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_skill::SqliteSkillRepository;
use tower::ServiceExt;

/// Build the router around a Matter runtime pinned to one state. The stub is
/// returned too, so a test can assert what the settings write asked it to do.
async fn make_app(
    matter: Option<Arc<StubMatterRuntime>>,
) -> (
    axum::Router,
    Option<Arc<StubMatterRuntime>>,
    tempfile::TempDir,
) {
    let (router, matter, _settings, tmp) = make_app_with_settings(matter).await;
    (router, matter, tmp)
}

/// As `make_app`, but hands back the settings repository too, so a test can
/// seed a row the API itself refuses to write.
async fn make_app_with_settings(
    matter: Option<Arc<StubMatterRuntime>>,
) -> (
    axum::Router,
    Option<Arc<StubMatterRuntime>>,
    Arc<MockSettingsRepository>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let db = Arc::new(db);

    let settings_repo = Arc::new(MockSettingsRepository::new());
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
        settings_repo: settings_repo.clone(),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(MockDeviceRegistry),
        matter: matter.clone().map(|m| m as Arc<dyn MatterRuntimePort>),
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
    });

    let router = build_router(state, std::path::PathBuf::from("pond-desktop/dist"));
    (router, matter, settings_repo, tmp)
}

fn authed(method: Method, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json");
    match body {
        Some(json) => builder
            .body(Body::from(serde_json::to_vec(&json).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// The commission request every test below sends.
fn commission_request() -> Request<Body> {
    authed(
        Method::POST,
        "/api/v1/devices/commission",
        Some(serde_json::json!({"code": "20202021"})),
    )
}

#[tokio::test]
async fn commissioning_while_matter_is_unavailable_does_not_send_the_user_looking_for_a_control() {
    let (app, _, _tmp) = make_app(Some(Arc::new(StubMatterRuntime::disabled()))).await;

    let response = app.oneshot(commission_request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let error = json_body(response).await["error"]
        .as_str()
        .unwrap()
        .to_string();

    // The message must not name a place: no Matter toggle exists in Settings or on the
    // Devices tab, so naming one sends the user hunting for a control that is not there.
    // `Disabled` is only reachable on a build with no Matter support compiled in, where
    // the honest answer names the build.
    for absent in ["Settings", "Devices tab", "turn it on"] {
        assert!(
            !error.contains(absent),
            "the message sends the user hunting for a control that does not exist ({absent}): \
             {error}"
        );
    }
    assert!(
        error.contains("compiled in"),
        "the message must say why Matter is unavailable: {error}"
    );
}

/// The headline bug: an enabled Matter whose controller is down used to report
/// itself as "not enabled", sending the user to flip a switch already on.
#[tokio::test]
async fn an_unreachable_controller_is_not_reported_as_disabled() {
    let (app, _, _tmp) = make_app(Some(Arc::new(StubMatterRuntime::unreachable(
        "connection refused",
    ))))
    .await;

    let response = app.oneshot(commission_request()).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::BAD_GATEWAY,
        "an unreachable dependency is a gateway failure, not 'feature off'"
    );

    let error = json_body(response).await["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains("connection refused"), "{error}");
    assert!(error.contains("ws://127.0.0.1:5580/giap"), "{error}");
    assert!(
        !error.contains("not enabled") && !error.contains("is off"),
        "it is enabled — do not say otherwise: {error}"
    );
}

/// Enabling starts a controller, which takes time. "Try again in a moment" is
/// actionable; "Matter is off" is wrong and sends the user to undo the toggle.
#[tokio::test]
async fn commissioning_while_starting_up_asks_the_user_to_wait() {
    let (app, _, _tmp) = make_app(Some(Arc::new(StubMatterRuntime::connecting()))).await;

    let response = app.oneshot(commission_request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let error = json_body(response).await["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains("starting up"), "{error}");
}

#[tokio::test]
async fn commissioning_works_once_the_runtime_is_connected() {
    let (app, _, _tmp) = make_app(Some(Arc::new(StubMatterRuntime::connected()))).await;

    let response = app.oneshot(commission_request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let body = json_body(response).await;
    assert_eq!(body["id"], "matter-7");
    assert_eq!(body["node_id"], 7);
}

/// A build with no Matter support at all is indistinguishable from "off" to the
/// user, and is reported that way rather than as a server error.
#[tokio::test]
async fn a_build_without_matter_reports_it_as_off() {
    let (app, _, _tmp) = make_app(None).await;

    let response = app
        .oneshot(authed(Method::GET, "/api/v1/matter/status", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["state"], "disabled");
    assert_eq!(body["enabled"], false);
}

/// The Devices tab polls this to watch the controller come up, so the flat
/// shape it branches on has to survive the round trip.
#[tokio::test]
async fn status_reports_the_state_and_its_failure_reason() {
    let (app, _, _tmp) = make_app(Some(Arc::new(StubMatterRuntime::unreachable(
        "no route to host",
    ))))
    .await;

    let response = app
        .oneshot(authed(Method::GET, "/api/v1/matter/status", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["state"], "unreachable");
    assert_eq!(body["enabled"], true);
    assert_eq!(body["error"], "no route to host");
    assert_eq!(body["url"], "ws://127.0.0.1:5580/giap");
}

/// The whole point of the change: saving the setting reconfigures the running
/// server. Before this, the flag was read once at startup and a user who turned
/// Matter on saw nothing happen until someone restarted the Pond.
#[tokio::test]
async fn saving_the_setting_reconciles_the_runtime_without_a_restart() {
    let runtime = Arc::new(StubMatterRuntime::disabled());
    let (app, matter, _tmp) = make_app(Some(runtime)).await;

    let response = app
        .oneshot(authed(
            Method::PUT,
            "/api/v1/settings",
            Some(serde_json::json!({
                "matter_ws_url": "ws://127.0.0.1:5580/giap",
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    assert_eq!(
        matter.unwrap().applied(),
        vec!["ws://127.0.0.1:5580/giap".to_string()],
        "the save must ask the runtime to converge"
    );
}

/// The URL is opened as a socket, so a typo is rejected at the save rather than
/// leaving the Matter section stuck reporting "unreachable" forever.
#[tokio::test]
async fn a_controller_address_that_is_not_a_websocket_url_is_rejected() {
    let runtime = Arc::new(StubMatterRuntime::disabled());
    let (app, matter, _tmp) = make_app(Some(runtime)).await;

    let response = app
        .oneshot(authed(
            Method::PUT,
            "/api/v1/settings",
            Some(serde_json::json!({
                "matter_enabled": true,
                "matter_ws_url": "127.0.0.1:5580",
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    assert!(
        matter.unwrap().applied().is_empty(),
        "a rejected save must not reconfigure the runtime"
    );
}

/// Deleting a Matter device has to remove it from the fabric first, so the same
/// state-aware refusal applies — and for the same reason must not misreport it.
#[tokio::test]
async fn deleting_a_matter_device_while_off_refuses_with_the_honest_reason() {
    let (app, _, _tmp) = make_app(Some(Arc::new(StubMatterRuntime::disabled()))).await;

    let response = app
        .oneshot(authed(Method::DELETE, "/api/v1/devices/matter-2", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let error = json_body(response).await["error"]
        .as_str()
        .unwrap()
        .to_string();
    // See the commissioning test above: there is no Matter control to point at
    // any more, so the refusal names the build instead of a tab.
    assert!(error.contains("compiled in"), "{error}");
    assert!(!error.contains("Devices tab"), "{error}");
}

/// Deleting a device behind a Matter hub is refused, and the refusal is the whole feature:
/// Matter commissions nodes, so decommissioning takes every sibling and dropping the row
/// alone leaves a zombie the controller re-announces. Checked before the Matter-state gate,
/// so an unreachable controller cannot turn a permanent refusal into a temporary "try later".
#[tokio::test]
async fn deleting_a_device_behind_a_hub_is_refused_and_names_the_hub() {
    let (app, _, _tmp) = make_app(Some(Arc::new(StubMatterRuntime::disabled()))).await;

    let response = app
        .oneshot(authed(Method::DELETE, "/api/v1/devices/matter-90-2", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);

    let body = json_body(response).await;
    let error = body["error"].as_str().unwrap().to_string();
    assert!(
        error.contains("provided by"),
        "the refusal must say where the device comes from: {error}"
    );
    assert!(
        error.contains("hub's own app"),
        "and where the user can actually remove it: {error}"
    );
    // The hub's id, so a client can offer to delete it instead of making the user
    // work out what `matter-90-2` is a child of.
    assert_eq!(body["hub_id"], "matter-90");
}

/// This endpoint takes a patch over the whole of Settings, so the Matter check must not
/// turn a bad stored controller address into a wall that blocks every unrelated save.
/// The fixture must really hold an invalid address, and seeding the repository directly is
/// the only way in: the API rejects a blank URL, so a row in that shape can only be legacy.
#[tokio::test]
async fn a_save_that_does_not_touch_matter_is_not_blocked_by_it() {
    let runtime = Arc::new(StubMatterRuntime::disabled());
    let (app, matter, settings_repo, _tmp) = make_app_with_settings(Some(runtime)).await;

    // The shape an install upgraded from the headless-knob era can be in, and
    // which no API call can produce: no address at all.
    let mut stored = settings_repo.get().await.unwrap();
    stored.matter_ws_url = String::new();
    settings_repo.update(&stored).await.unwrap();

    // An unrelated edit still goes through, rather than being refused because
    // of a field the caller never touched.
    let response = app
        .oneshot(authed(
            Method::PUT,
            "/api/v1/settings",
            Some(serde_json::json!({"home_name": "The Nest"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // ...and it does NOT reconcile Matter. The reconciler treats "enabled but not yet
    // Connected" as needing a restart, so an unconditional `apply` would tear down an
    // in-flight controller install. `saveMatter` sends both Matter keys, so a genuine
    // retry is still a `touches_matter` save.
    assert!(
        matter.unwrap().applied().is_empty(),
        "an unrelated settings save reconciled Matter, which restarts an \
         in-flight controller install"
    );
}
