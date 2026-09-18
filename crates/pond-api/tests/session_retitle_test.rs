//! `POST /api/v1/sessions/retitle`, the attended half of conversation naming.
//!
//! The gate, normalisation and persistence are unit-tested elsewhere; what only
//! a route test reaches is that the button does something and that a refusal
//! says which kind.
//!
//! # The sweep's own rules are no longer asserted here, and that is deliberate
//!
//! This route used to run the sweep inline — up to twenty model calls inside
//! the request handler, taking no lane slot — so its per-conversation rules
//! were reachable through it and were tested through it. It now asks the
//! titling job to run its next pass and answers, so those rules are the
//! titling job's and are tested where they live: `session_title.rs`'s own
//! suite covers a typed name never being overwritten (and costing no inference
//! to refuse), a name that still fits not being rebuilt, and a conversation too
//! short to describe. What is asserted here instead is the property that
//! replaced them — **the request itself decodes nothing.**

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

/// Answers every naming request with the same line, and counts how often it was
/// asked — the count is how the tests prove a refusal cost no inference.
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

/// A lane that records what it was asked to wake, and answers how it was told.
///
/// `snapshot` is unreachable from this route and panics rather than returning a
/// plausible empty one: a snapshot nobody asked for would be a silent way for a
/// future handler to read state this stub never had.
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

/// The same app, with a lane whose wakes the caller can read back.
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

/// The press asks the titling job to run, and says so.
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

/// THE PROPERTY THAT REPLACED THE SWEEP'S OWN TESTS.
///
/// This route ran up to twenty model calls inside the request handler, holding
/// no lane slot — so it could decode beside whichever background job already
/// had the machine, which is the one thing the lane exists to prevent. It also
/// could not finish: the desktop's default client timeout is 30 s, and twenty
/// titles on the Orin is minutes.
///
/// The provider's call count is how that is asserted rather than described. A
/// handler that quietly went back to doing the work itself would still answer
/// `started: true`, and only this number would notice.
#[tokio::test]
async fn the_press_decodes_nothing_in_the_request() {
    let provider = StubProvider::new("A name nobody asked for");
    let (app, storage, _tmp, _) =
        make_app_with_lane(Some(provider.clone()), Some(WakeOutcome::Woken)).await;
    // Three conversations all sitting on their fallback names: the sweep would
    // have renamed every one of them, inline, before answering.
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

/// A pond whose titling loop never spawned has nothing to wake, and the button
/// must be able to say that rather than claim a pass it did not start.
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

/// And a process with no lane at all is a third answer, not the second one.
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

/// Checked in the handler rather than left to the job, because the job's answer
/// to "no model configured" is to skip its tick in silence — right for a
/// background loop and useless to somebody who just pressed a button.
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

/// The deliberate asymmetry, and the one worth stating plainly: the sweep
/// refuses to touch a name somebody typed, and this replaces it. Asking for
/// one named conversation is consent about that conversation, and a button
/// that quietly declined would be indistinguishable from a broken one.
#[tokio::test]
async fn asking_for_one_conversation_replaces_even_a_name_typed_by_hand() {
    let provider = StubProvider::new("Wake word fires twice on the Jetson");
    let (app, storage, _tmp) = make_app(Some(provider.clone())).await;

    seed(&storage, "sess-1", 8).await;
    storage
        .update_title("sess-1", "Jetson deploy notes".to_string())
        .await
        .unwrap();

    // The sweep leaves it alone...
    // The sweep's own refusal is no longer reachable through a route -- it
    // belongs to the titling job now -- and is asserted where it lives, in
    // `session_title.rs`'s `a_user_named_session_costs_no_inference_at_all`.
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

    // Nothing has changed since that name was written, so the sweep declines.
    // As above: the sweep declining a name that still fits is
    // `session_title.rs`'s business, and its suite asserts it. What this test
    // is for is the other half of the asymmetry -- that asking for ONE
    // conversation rebuilds it anyway.

    let (status, body) = retitle_one(&app, "sess-1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["outcome"], "retitled");
    assert_eq!(body["title"], "A freshly considered name");
}

/// Forcing overrides permission, not possibility. A two-message conversation
/// has nothing to describe, and no amount of asking changes that.
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

/// The reply is a contract with the desktop client, which types every field.
///
/// It used to carry seven counters and a nested `skipped` breakdown, because it
/// did the work and could count it. It carries two fields now, and the test
/// asserts the ABSENCE of the old ones as well as the presence of the new: a
/// handler that answered with both shapes would let the client keep reading a
/// count that no longer means anything.
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
