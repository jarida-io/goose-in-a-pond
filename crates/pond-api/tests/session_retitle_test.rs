//! `POST /api/v1/sessions/retitle`, the attended half of conversation naming.
//! The gate, normalisation and persistence are unit-tested elsewhere; what only a
//! route test reaches is that the button does something, that the manual path
//! still refuses to overwrite a typed name, and that a refusal says which one.

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
use pond_core::user_data::ports::session_storage::SessionStorage;
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

async fn make_app(
    provider: Option<Arc<dyn LlmProvider>>,
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

#[tokio::test]
async fn a_press_renames_a_conversation_still_on_its_fallback_name() {
    let provider = StubProvider::new("Wake word fires twice on the Jetson");
    let (app, storage, _tmp) = make_app(Some(provider.clone())).await;

    seed(&storage, "sess-1", 8).await;
    storage
        .set_derived_title("sess-1", "so i was wondering whether 0")
        .await
        .unwrap();

    let (status, body) = retitle(&app).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["renamed_count"], 1);
    assert_eq!(body["renamed"][0]["session_id"], "sess-1");
    assert_eq!(
        body["renamed"][0]["title"],
        "Wake word fires twice on the Jetson"
    );

    assert_eq!(
        storage
            .get_session("sess-1")
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("Wake word fires twice on the Jetson"),
        "the new name must actually be persisted, not just reported"
    );
    // Provenance recorded, so the automatic pass knows this one is now current.
    assert_eq!(
        storage.get_title_provenance("sess-1").await.unwrap().0,
        Some("model".to_string())
    );
}

/// Pressing a button must never destroy a name somebody chose. Asserting the
/// model was not even asked makes the guarantee cheap as well as safe.
#[tokio::test]
async fn a_press_never_overwrites_a_name_somebody_typed() {
    let provider = StubProvider::new("Something else entirely");
    let (app, storage, _tmp) = make_app(Some(provider.clone())).await;

    seed(&storage, "sess-1", 12).await;
    storage
        .update_title("sess-1", "Jetson deploy notes".to_string())
        .await
        .unwrap();

    let (status, body) = retitle(&app).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["renamed_count"], 0);
    assert_eq!(body["skipped"]["user_named"], 1);
    assert_eq!(
        provider.calls(),
        0,
        "a protected conversation must cost no inference"
    );
    assert_eq!(
        storage
            .get_session("sess-1")
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("Jetson deploy notes")
    );
}

#[tokio::test]
async fn a_pass_that_renames_nothing_says_which_kind_of_nothing() {
    let provider = StubProvider::new("A perfectly good name");
    let (app, storage, _tmp) = make_app(Some(provider.clone())).await;

    // Too short to describe — the six-word fallback already covers this.
    seed(&storage, "sess-short", 1).await;

    let (status, body) = retitle(&app).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["renamed_count"], 0);
    assert_eq!(body["considered"], 1);
    assert_eq!(body["skipped"]["too_short"], 1);
    assert_eq!(provider.calls(), 0);
}

#[tokio::test]
async fn the_pond_does_not_name_its_own_background_conversations() {
    let provider = StubProvider::new("A name nobody asked for");
    let (app, storage, _tmp) = make_app(Some(provider.clone())).await;

    // The prefix `sched-` marks a conversation the pond opened for itself.
    seed(&storage, "sched-nightly-1700000000", 10).await;
    storage
        .set_derived_title("sched-nightly-1700000000", "so i was wondering whether 0")
        .await
        .unwrap();

    let (status, body) = retitle(&app).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["considered"], 0,
        "it should not even have been looked at"
    );
    assert_eq!(provider.calls(), 0);
}

#[tokio::test]
async fn without_a_model_the_button_says_so_rather_than_failing_quietly() {
    let (app, storage, _tmp) = make_app(None).await;
    seed(&storage, "sess-1", 8).await;

    let (status, body) = retitle(&app).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body["error"].as_str().unwrap_or_default().contains("model"),
        "the reason must name the missing piece, got {body}"
    );
}

// ── The per-conversation button: obeys rather than protects ─────────────────

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
    let (_, sweep) = retitle(&app).await;
    assert_eq!(sweep["skipped"]["user_named"], 1);
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
    let (_, sweep) = retitle(&app).await;
    assert_eq!(sweep["skipped"]["still_current"], 1);

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
/// A rename that reported nothing countable would leave the button unable to
/// say what it did.
#[tokio::test]
async fn the_reply_carries_every_field_the_client_reads() {
    let provider = StubProvider::new("A name");
    let (app, storage, _tmp) = make_app(Some(provider)).await;
    seed(&storage, "sess-1", 8).await;

    let (_, body) = retitle(&app).await;

    for key in [
        "renamed",
        "renamed_count",
        "considered",
        "capped",
        "unusable",
        "failed",
        "skipped",
    ] {
        assert!(body.get(key).is_some(), "missing {key} in {body}");
    }
    for key in [
        "user_named",
        "still_current",
        "too_short",
        "unknown_provenance",
    ] {
        assert!(
            body["skipped"].get(key).is_some(),
            "missing skipped.{key} in {body}"
        );
    }
    assert!(body["renamed"].is_array());
}
