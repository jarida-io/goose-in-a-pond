//! Index health/rebuild routes. Rebuild clears rows because the sweep re-embeds only absent ones.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::context::vector_index::{Corpus, VectorEntry, VectorIndex};
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_vector_index::SqliteVectorIndex;
use serde_json::Value;
use sqlx::{Pool, Sqlite};
use tower::ServiceExt;

// ── Stubs ────────────────────────────────────────────────────────────────────

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

struct NoDevices;

#[async_trait::async_trait]
impl DeviceRegistry for NoDevices {
    async fn register(&self, _: RegisterDeviceRequest) -> anyhow::Result<Device> {
        anyhow::bail!("not used")
    }
    async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        Ok(vec![])
    }
    async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> {
        Ok(None)
    }
    async fn unregister(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn heartbeat(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Named, with a fixed width: the route reports both, and a mismatch in either is a defect.
const STUB_MODEL: &str = "stub-embed-v1";

struct StubEmbedder;

#[async_trait::async_trait]
impl EmbeddingProvider for StubEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(vec![0.0; 4])
    }
    fn dimensions(&self) -> usize {
        4
    }
    fn model_id(&self) -> String {
        STUB_MODEL.to_string()
    }
}

// ── Harness ──────────────────────────────────────────────────────────────────

struct Harness {
    app: axum::Router,
    system: Pool<Sqlite>,
    index: Arc<SqliteVectorIndex>,
    reindex: Arc<tokio::sync::Notify>,
    profiles: Arc<SqliteProfileRepository>,
    _tmp: tempfile::TempDir,
}

/// Pond shapes: no index (CLI), index but no embedder (`embedding_provider = "none"`), or live.
async fn make_app(wire_index: bool, wire_embedder: bool) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let system = db.system.clone();
    let profiles = Arc::new(SqliteProfileRepository::new(system.clone()));
    // Built even when unwired, so a test can seed vectors into a pond that reports no index.
    let index = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));
    // Also held by the harness, so a test can prove the route wakes the sweep.
    let reindex = Arc::new(tokio::sync::Notify::new());
    let hs = MockHandshake::new();
    hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        transcribe_audio: None,
        session_storage: Arc::new(
            pond_infra::sqlite_session_storage::SqliteSessionStorage::new(system.clone()),
        ),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: profiles.clone(),
        device_registry: Arc::new(NoDevices),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: wire_embedder
            .then(|| Arc::new(StubEmbedder) as Arc<dyn EmbeddingProvider + Send + Sync>),
        vector_index: wire_index.then(|| index.clone() as Arc<dyn VectorIndex>),
        // Only with an embedder, as in `main.rs`, which spawns the sweep only when one exists.
        index_reindex: (wire_index && wire_embedder).then(|| reindex.clone()),
        account_sync: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        face_recognition: None,
        prompt_template_dir: None,
        model_repo: None,
        data_dir: None,
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
        security_policy: None,
        tool_dispatcher: None,
        event_bus: None,
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_sender: None,
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

    Harness {
        app: build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        system,
        index,
        reindex,
        profiles,
        _tmp: tmp,
    }
}

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// A real member: `memory_fragments.profile_id` is an enforced foreign key.
async fn member(h: &Harness) -> String {
    h.profiles
        .create(CreateProfileRequest {
            display_name: "Liz".to_string(),
            avatar_emoji: "*".to_string(),
        })
        .await
        .unwrap()
        .id
}

/// A live memory via SQL: `memory_repo` here is a mock whose rows never reach the table.
async fn add_memory(h: &Harness, id: &str, profile: &str) {
    sqlx::query(
        "INSERT INTO memory_fragments (id, profile_id, content, source, tags, created_at, \
         access_count, lifecycle) VALUES (?, ?, 'x', 'chat', '[]', datetime('now'), 0, 'active')",
    )
    .bind(id)
    .bind(profile)
    .execute(&h.system)
    .await
    .unwrap();
}

/// A session with a rolling summary and no owner, which `liveness_sql(Summary)` excludes.
async fn add_unattributed_summary(h: &Harness, id: &str) {
    sqlx::query(
        "INSERT INTO sessions (id, created_at, profile_id, rolling_summary, \
         rolling_summary_updated_at) VALUES (?, datetime('now'), NULL, 'we agreed on Tuesday', \
         datetime('now'))",
    )
    .bind(id)
    .execute(&h.system)
    .await
    .unwrap();
}

async fn add_vector(h: &Harness, corpus: Corpus, row_id: &str, model_id: &str) {
    h.index
        .upsert(&VectorEntry {
            corpus,
            row_id: row_id.to_string(),
            chunk_ix: 0,
            chunk_span: None,
            model_id: model_id.to_string(),
            vector: vec![0.1, 0.2, 0.3, 0.4],
            source_rev: None,
        })
        .await
        .unwrap();
}

// ── HTTP helpers ─────────────────────────────────────────────────────────────

async fn send(app: &axum::Router, method: Method, uri: &str, token: bool) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if token {
        req = req.header("Authorization", "Bearer test-token");
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn health(h: &Harness) -> (StatusCode, Value) {
    send(&h.app, Method::GET, "/api/v1/context/index/health", true).await
}

async fn rebuild(h: &Harness) -> (StatusCode, Value) {
    send(&h.app, Method::POST, "/api/v1/context/index/rebuild", true).await
}

/// The row for one corpus; panics if absent, since every corpus must be represented.
fn corpus_row<'a>(body: &'a Value, corpus: &str) -> &'a Value {
    body["corpora"]
        .as_array()
        .unwrap_or_else(|| panic!("no corpora array in {body}"))
        .iter()
        .find(|row| row["corpus"] == corpus)
        .unwrap_or_else(|| panic!("{corpus} is missing from {body}"))
}

// ── A pond with no index answers, rather than failing ──────────────────────

#[tokio::test]
async fn a_pond_with_no_index_is_a_state_the_route_can_describe() {
    let h = make_app(false, false).await;

    let (status, body) = health(&h).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a pond with embeddings switched off is a legitimate configuration, and an error here \
         makes it indistinguishable from a broken one: {body}"
    );
    assert_eq!(body["indexed"], false);
    assert!(
        body["reason"].as_str().unwrap_or("").len() > 20,
        "the answer has to say WHY there is nothing to report, or the UI can only render a \
         shrug: {body}"
    );
    assert_eq!(body["corpora"].as_array().map(|a| a.len()), Some(0));

    let (status, body) = rebuild(&h).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["cleared"], 0,
        "there is no index, so nothing was cleared, and saying otherwise would send an operator \
         looking for an effect that never happened: {body}"
    );
}

#[tokio::test]
async fn an_index_with_no_embedder_reports_that_nothing_can_be_indexed() {
    let h = make_app(true, false).await;

    let (status, body) = health(&h).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["indexed"], false,
        "coverage is 'of the rows carrying a vector from THIS model, how many' -- with no model \
         configured there is nothing to ask about, and reporting every row as missing describes \
         a wiped index and a deliberately-disabled one identically: {body}"
    );
    assert_eq!(body["model_id"], Value::Null);
}

// ── Every corpus is its own row ────────────────────────────────────────────

#[tokio::test]
async fn the_coverage_number_leaves_the_process() {
    let h = make_app(true, true).await;
    let liz = member(&h).await;

    // Four memories: one current, one under a stale model (needs re-embedding), two unindexed.
    for id in ["m1", "m2", "m3", "m4"] {
        add_memory(&h, id, &liz).await;
    }
    add_vector(&h, Corpus::Memory, "m1", STUB_MODEL).await;
    add_vector(&h, Corpus::Memory, "m2", "some-older-embedder").await;

    let (status, body) = health(&h).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["indexed"], true);
    assert_eq!(body["model_id"], STUB_MODEL);
    assert_eq!(body["dims"], 4);
    assert_eq!(body["rows"], 4);
    assert_eq!(body["matching"], 1);
    assert_eq!(body["mismatched"], 1);
    assert_eq!(body["missing"], 2);
    assert_eq!(
        body["coverage"], 0.25,
        "the fraction is what a person actually reads. A pond at 2% looked fine for six phases \
         because nothing ever computed this: {body}"
    );

    let memory = corpus_row(&body, "memory");
    assert_eq!(memory["rows"], 4);
    assert_eq!(memory["indexed_rows"], 1);
    assert_eq!(memory["mismatched"], 1);
    assert_eq!(memory["missing_rows"], 2);
    assert_eq!(memory["coverage"], 0.25);

    // No attributed session, so the summary corpus cannot populate: its own row, at zero.
    let summary = corpus_row(&body, "summary");
    assert_eq!(summary["rows"], 0);
    assert_eq!(
        summary["coverage"],
        Value::Null,
        "zero of zero is neither 0% nor 100%, and both readings mislead: one shows a permanent \
         red figure on a pond with nothing to index, the other a green 100% on a corpus that is \
         structurally unable to answer anything: {body}"
    );
    assert_eq!(corpus_row(&body, "context")["rows"], 0);
}

/// The refill sweep is idle-gated, so without a wake it would not run while the user watches.
#[tokio::test]
async fn rebuilding_wakes_the_sweep_that_refills_it() {
    let h = make_app(true, true).await;

    // Listen BEFORE the request: a later listener could pass on `Notify`'s stored permit alone.
    let listener = h.reindex.clone();
    let woken = tokio::spawn(async move { listener.notified().await });
    tokio::task::yield_now().await;

    let (status, body) = rebuild(&h).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["refilling"], true,
        "the answer has to say the refill was asked for, or a caller cannot tell this pond \
         apart from one with no sweep to wake: {body}"
    );

    tokio::time::timeout(std::time::Duration::from_secs(2), woken)
        .await
        .expect("the sweep was never woken, so the index stays empty until the next pass")
        .expect("waiter task panicked");
}

#[tokio::test]
async fn a_pond_with_no_sweep_clears_and_admits_nothing_will_refill_it() {
    let h = make_app(true, false).await;
    let (status, body) = rebuild(&h).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["indexed"], true);
    assert_eq!(
        body["refilling"], false,
        "no embedder means no sweep in this process; claiming a refill would be a lie that \
         reads as success: {body}"
    );
}

/// Empty and excluded corpora both show zero qualifying rows but need opposite fixes.
#[tokio::test]
async fn a_corpus_excluded_by_its_predicate_is_not_reported_as_an_empty_one() {
    let h = make_app(true, true).await;
    let liz = member(&h).await;

    // A healthy corpus alongside: it is what makes the overall figure look fine.
    add_memory(&h, "m1", &liz).await;
    add_vector(&h, Corpus::Memory, "m1", STUB_MODEL).await;

    for id in ["s1", "s2", "s3"] {
        add_unattributed_summary(&h, id).await;
    }

    let (status, body) = health(&h).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let summary = corpus_row(&body, "summary");
    assert_eq!(
        summary["rows"], 0,
        "the predicate excludes all three, so nothing qualifies: {body}"
    );
    assert_eq!(
        summary["source_rows"], 3,
        "the table is NOT empty, and that is the only thing separating a corpus \
         waiting for data from a corpus no query can reach: {body}"
    );
    assert_eq!(
        summary["structurally_excluded"], true,
        "rows == 0 with source_rows > 0 is a predicate bug, not a backlog. No amount of \
         embedding repairs it, so it must not be reported as though embedding would: {body}"
    );

    // Control: a genuinely empty corpus must NOT raise the same flag.
    let context = corpus_row(&body, "context");
    assert_eq!(context["rows"], 0);
    assert_eq!(context["source_rows"], 0);
    assert_eq!(
        context["structurally_excluded"], false,
        "an empty corpus is waiting for data and is not broken: {body}"
    );

    assert_eq!(
        body["coverage"], 1.0,
        "this is the trap, pinned deliberately. Coverage counts only qualifying rows, so an \
         excluded corpus cannot pull it down -- which is why the per-corpus flag, not the \
         headline fraction, is what a reader has to be shown: {body}"
    );
}

// ── Rebuild says what it cleared ───────────────────────────────────────────

#[tokio::test]
async fn rebuilding_reports_what_it_cleared_and_leaves_the_index_empty() {
    let h = make_app(true, true).await;
    let liz = member(&h).await;
    add_memory(&h, "m1", &liz).await;

    add_vector(&h, Corpus::Memory, "m1", STUB_MODEL).await;
    add_vector(&h, Corpus::Context, "c1", STUB_MODEL).await;
    add_vector(&h, Corpus::Context, "c2", STUB_MODEL).await;

    let (status, body) = rebuild(&h).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["cleared"], 3,
        "an operator affordance that reports nothing cannot be checked, and this one exists \
         because a changed embedder leaves rows that score plausibly and are wrong: {body}"
    );
    assert_eq!(corpus_row(&body, "memory")["cleared"], 1);
    assert_eq!(corpus_row(&body, "context")["cleared"], 2);
    assert_eq!(
        corpus_row(&body, "summary")["cleared"],
        0,
        "a corpus is listed at zero rather than omitted, for the same reason the health rows are"
    );

    // Now "missing", which the sweep re-embeds; the source row survives, so nothing is lost.
    let (_, after) = health(&h).await;
    assert_eq!(after["matching"], 0);
    assert_eq!(after["missing"], 1);
    assert_eq!(corpus_row(&after, "memory")["rows"], 1);

    let (_, again) = rebuild(&h).await;
    assert_eq!(
        again["cleared"], 0,
        "the second rebuild had nothing to clear and must say so, not repeat the first count"
    );
}

// ── Both routes require a token ────────────────────────────────────────────

#[tokio::test]
async fn the_index_routes_are_protected() {
    let h = make_app(true, true).await;

    for (method, uri) in [
        (Method::GET, "/api/v1/context/index/health"),
        (Method::POST, "/api/v1/context/index/rebuild"),
    ] {
        let (status, _) = send(&h.app, method.clone(), uri, false).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} answered an anonymous caller. The counts say how much of a \
             household's memory exists, and the rebuild throws work at the machine"
        );
    }
}
