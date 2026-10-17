//! `POST /api/v1/sessions/retitle` at the route level; the gate itself is unit-tested elsewhere.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::session::SessionMessage;
use pond_core::user_data::mocks::mock_device_registry::MockDeviceRegistry;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::lane_control::{LaneControl, WakeOutcome};
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::services::inference_lane::LaneJob;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::onboarding::SqlxOnboardingRepository;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_skill::SqliteSkillRepository;
use serde_json::Value;
use tower::ServiceExt;

/// Fixed reply, counting calls so tests can prove a refusal cost no inference.
struct StubProvider {
    reply: String,
    calls: AtomicUsize,
}

impl StubProvider {
    fn new(reply: &str) -> Arc<Self> {
        Arc::new(Self {
            reply: reply.to_string(),
            calls: AtomicUsize::new(0),
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmProvider for StubProvider {
    async fn complete(&self, _system: &str, _messages: Vec<ChatMessage>) -> Result<ChatMessage> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ChatMessage::assistant(self.reply.clone()))
    }
    fn model_name(&self) -> String {
        "stub".to_string()
    }
}

/// `snapshot` panics rather than return a plausible empty state no route should read.
struct StubLane {
    woken: Arc<std::sync::Mutex<Vec<LaneJob>>>,
    outcome: WakeOutcome,
}

#[async_trait::async_trait]
impl LaneControl for StubLane {
    async fn snapshot(&self) -> pond_core::user_data::ports::lane_control::LaneSnapshot {
        unreachable!("the retitle route does not read the lane's state")
    }
    async fn wake(&self, job: LaneJob) -> WakeOutcome {
        self.woken.lock().unwrap().push(job);
        self.outcome
    }
}

async fn make_app(
    provider: Option<Arc<dyn LlmProvider>>,
) -> (axum::Router, Arc<SqliteSessionStorage>, tempfile::TempDir) {
    let (app, storage, tmp, _) = make_app_with_lane(provider, None).await;
    (app, storage, tmp)
}

async fn make_app_with_lane(
    provider: Option<Arc<dyn LlmProvider>>,
    outcome: Option<WakeOutcome>,
) -> (
    axum::Router,
    Arc<SqliteSessionStorage>,
    tempfile::TempDir,
    Arc<std::sync::Mutex<Vec<LaneJob>>>,
) {
    let woken = Arc::new(std::sync::Mutex::new(Vec::new()));
    let lane: Option<Arc<dyn LaneControl>> = outcome.map(|outcome| {
        Arc::new(StubLane {
            woken: woken.clone(),
            outcome,
        }) as Arc<dyn LaneControl>
    });
    let (app, storage, tmp) = build_app(provider, lane).await;
    (app, storage, tmp, woken)
}

async fn build_app(
    provider: Option<Arc<dyn LlmProvider>>,
    lane: Option<Arc<dyn LaneControl>>,
) -> (axum::Router, Arc<SqliteSessionStorage>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let db = Arc::new(db);

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let storage = Arc::new(SqliteSessionStorage::new(pool.clone()));

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
        session_storage: storage.clone(),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(provider)),
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
        lane,
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
    });

    let router = build_router(state, std::path::PathBuf::from("pond-desktop/dist"));
    (router, storage, tmp)
}

/// A conversation long enough to be worth naming.
async fn seed(storage: &SqliteSessionStorage, session_id: &str, count: usize) {
    storage
        .create_session(session_id.to_string())
        .await
        .unwrap();
    for i in 0..count {
        let msg = if i % 2 == 0 {
            ChatMessage::user(format!("so i was wondering whether {i}"))
        } else {
            ChatMessage::assistant(format!("answer {i}"))
        };
        storage
            .add_message(
                session_id.to_string(),
                SessionMessage::new(format!("{session_id}-m{i}"), session_id.to_string(), msg),
            )
            .await
            .unwrap();
    }
}

async fn retitle(app: &axum::Router) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/sessions/retitle")
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn a_press_asks_the_titling_job_for_its_next_pass() {
    let provider = StubProvider::new("Wake word fires twice");
    let (app, storage, _tmp, woken) =
        make_app_with_lane(Some(provider.clone()), Some(WakeOutcome::Woken)).await;
    seed(&storage, "sess-1", 8).await;

    let (status, body) = retitle(&app).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["started"], true);
    assert_eq!(*woken.lock().unwrap(), vec![LaneJob::Titling]);
}

#[tokio::test]
async fn the_press_decodes_nothing_in_the_request() {
    let provider = StubProvider::new("A name nobody asked for");
    let (app, storage, _tmp, _) =
        make_app_with_lane(Some(provider.clone()), Some(WakeOutcome::Woken)).await;
    // Fallback-named sessions, which an inline sweep would retitle before answering.
    for id in ["sess-1", "sess-2", "sess-3"] {
        seed(&storage, id, 8).await;
    }

    let (status, _) = retitle(&app).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        provider.calls(),
        0,
        "the request must not decode; the lane runs the pass"
    );
}

#[tokio::test]
async fn a_job_with_no_loop_says_so_rather_than_claiming_it_started() {
    let provider = StubProvider::new("unused");
    let (app, _storage, _tmp, _) =
        make_app_with_lane(Some(provider), Some(WakeOutcome::NotPresent)).await;

    let (status, body) = retitle(&app).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["started"], false);
    assert!(
        body["reason"].as_str().is_some_and(|r| r.contains("loop")),
        "{body}"
    );
}

#[tokio::test]
async fn a_process_with_no_lane_is_a_different_answer_from_a_missing_loop() {
    let provider = StubProvider::new("unused");
    let (app, _storage, _tmp) = make_app(Some(provider)).await;

    let (status, body) = retitle(&app).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["started"], false);
    assert!(
        body["reason"]
            .as_str()
            .is_some_and(|r| r.contains("no inference lane")),
        "{body}"
    );
}

/// Checked in the handler because the job silently skips a tick with no model.
#[tokio::test]
async fn without_a_model_the_button_says_so_rather_than_failing_quietly() {
    let (app, _storage, _tmp, woken) = make_app_with_lane(None, Some(WakeOutcome::Woken)).await;

    let (status, body) = retitle(&app).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].as_str().is_some_and(|e| e.contains("model")));
    assert!(
        woken.lock().unwrap().is_empty(),
        "nothing should be woken to do work it has no model for"
    );
}

async fn retitle_one(app: &axum::Router, session_id: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/v1/sessions/{session_id}/retitle"))
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// Deliberately unlike the sweep: asking for one conversation is consent to rename it.
#[tokio::test]
async fn asking_for_one_conversation_replaces_even_a_name_typed_by_hand() {
    let provider = StubProvider::new("Wake word fires twice on the Jetson");
    let (app, storage, _tmp) = make_app(Some(provider.clone())).await;

    seed(&storage, "sess-1", 8).await;
    storage
        .update_title("sess-1", "Jetson deploy notes".to_string())
        .await
        .unwrap();

    // The sweep leaves it alone (asserted in `session_title.rs`)...
    assert_eq!(provider.calls(), 0);

    // ...and asking for this one specifically does not.
    let (status, body) = retitle_one(&app, "sess-1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["outcome"], "retitled");
    assert_eq!(body["title"], "Wake word fires twice on the Jetson");
    assert_eq!(
        storage
            .get_session("sess-1")
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("Wake word fires twice on the Jetson")
    );
}

#[tokio::test]
async fn asking_for_one_conversation_rebuilds_a_name_that_still_fits() {
    let provider = StubProvider::new("A freshly considered name");
    let (app, storage, _tmp) = make_app(Some(provider.clone())).await;

    seed(&storage, "sess-1", 8).await;
    storage
        .set_generated_title("sess-1", "An older name", "sess-1-m7")
        .await
        .unwrap();

    // The sweep would decline this still-fitting name (`session_title.rs` asserts it).

    let (status, body) = retitle_one(&app, "sess-1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["outcome"], "retitled");
    assert_eq!(body["title"], "A freshly considered name");
}

/// Forcing overrides permission, not possibility.
#[tokio::test]
async fn asking_for_a_conversation_too_short_to_describe_says_so() {
    let provider = StubProvider::new("A name");
    let (app, storage, _tmp) = make_app(Some(provider.clone())).await;
    seed(&storage, "sess-1", 1).await;

    let (status, body) = retitle_one(&app, "sess-1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["outcome"], "skipped");
    assert_eq!(body["reason"], "too_short");
    assert_eq!(body["title"], Value::Null);
    assert_eq!(provider.calls(), 0);
}

#[tokio::test]
async fn asking_for_a_conversation_that_does_not_exist_is_a_404() {
    let provider = StubProvider::new("A name");
    let (app, _storage, _tmp) = make_app(Some(provider)).await;

    let (status, _) = retitle_one(&app, "no-such-session").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The desktop client types every field of this reply.
#[tokio::test]
async fn the_reply_carries_every_field_the_client_reads_and_no_stale_ones() {
    let provider = StubProvider::new("A name");
    let (app, storage, _tmp, _) =
        make_app_with_lane(Some(provider), Some(WakeOutcome::Woken)).await;
    seed(&storage, "sess-1", 8).await;

    let (_, body) = retitle(&app).await;

    assert!(body["started"].is_boolean(), "{body}");
    for stale in [
        "renamed",
        "renamed_count",
        "considered",
        "capped",
        "unusable",
        "failed",
        "skipped",
    ] {
        assert!(
            body.get(stale).is_none(),
            "{stale} is a count this reply cannot honestly carry: {body}"
        );
    }
}
