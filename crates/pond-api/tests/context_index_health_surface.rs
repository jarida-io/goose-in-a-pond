//! The index's coverage leaves the process, and can be repaired from outside it. Embeddings
//! off must answer, not 500; every corpus is its own row, at zero with no coverage fraction
//! when nothing qualifies, because an average hid one that could never populate; rebuild
//! reports what it cleared, since the sweep re-embeds only ABSENT rows; both need a token.

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

/// Named, and of a fixed width, because the route reports both and a mismatch
/// against either is the defect this surface exists to expose. It embeds
/// nothing: no test here asks it to, and a health count is a `JOIN` over stored
/// rows rather than anything the model is asked to compute.
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

/// The two knobs that decide which of the three shapes a pond is in: no index at
/// all (the CLI paths and every other test in this crate), an index with nobody
/// to fill it (`embedding_provider = "none"`, a real configuration), or a live
/// one.
async fn make_app(wire_index: bool, wire_embedder: bool) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let system = db.system.clone();
    let profiles = Arc::new(SqliteProfileRepository::new(system.clone()));
    // Built whether or not it is wired into `AppState`, so a test can seed
    // vectors into a pond whose route is expected to report no index.
    let index = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));
    // Held by the harness as well as the state, so a test can wait on it and
    // prove the route actually wakes the sweep rather than merely holding a
    // handle it never uses.
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
        // Present only when a sweep would exist to wake, which in production
        // means an embedder: `main.rs` spawns the sweep inside the same
        // `if let Some(provider)`. Wiring it whenever the index is present would
        // make this harness claim a refill on a pond where nothing can refill.
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

/// `memory_fragments.profile_id` is a real foreign key and `db.rs` enables
/// `foreign_keys` on every connection, so a fixture that invents a member id is
/// rejected. Create the member first.
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

/// A live memory, written as SQL rather than through `memory_repo`: the health
/// query counts SOURCE rows in `pond_system.db`, and this crate's harness holds
/// an in-memory mock repository whose rows never reach the table being counted.
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

/// A session carrying a real rolling summary and NO owner. `liveness_sql(Summary)` requires
/// `profile_id IS NOT NULL`, so such a session is excluded from every count that uses the
/// predicate: on the live pond 27 of them existed and the corpus reported zero qualifying
/// rows, which reads exactly like an empty corpus.
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

/// The row for one corpus, by name. Absence is a failure rather than a `None`:
/// the port's contract is that every corpus is represented, because a corpus
/// missing from the answer is a corpus nobody can see is broken.
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

    // Four live memories: one indexed under the current model, one under an
    // older one (present, unusable, repaired by RE-embedding), two with no
    // vector at all (repaired by embedding). Three different repairs, which is
    // why the answer keeps them apart rather than reporting one shortfall.
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

    // The claim the per-corpus shape exists for. This pond has no attributed
    // session, so the summary corpus cannot populate AT ALL -- and it says so as
    // its own row, at zero, instead of being averaged into the memory corpus's
    // number and disappearing.
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

/// Clearing the index is only half of "reindex"; the other half is the refilling sweep,
/// which is idle-gated and so will not normally run while the person who pressed the button
/// is still there. Without the wake, the route empties the panel and leaves it empty until
/// the next scheduled pass, which on a pond nobody restarts looks like doing nothing.
#[tokio::test]
async fn rebuilding_wakes_the_sweep_that_refills_it() {
    let h = make_app(true, true).await;

    // Subscribed BEFORE the request. `Notify` only holds a permit for a
    // `notify_one` with no waiter, so a test that starts listening afterwards
    // can pass on the stored permit alone and would keep passing if the route
    // fired at the wrong moment.
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

/// A pond with no sweep to wake still clears, and says it did not refill. This is the CLI
/// shape, where `refilling: true` would be a lie that reads as success: the caller would
/// wait for a rebuild that nothing in the process is going to perform.
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

/// An empty corpus and an EXCLUDED corpus both report zero qualifying rows and need opposite
/// fixes, so the surface has to tell them apart. A pond with no sessions at all cannot show
/// the difference: 27 sessions held a rolling summary no query could reach and the route
/// still read 100% covered, because 0 of 0 does not drag an average down.
#[tokio::test]
async fn a_corpus_excluded_by_its_predicate_is_not_reported_as_an_empty_one() {
    let h = make_app(true, true).await;
    let liz = member(&h).await;

    // A healthy corpus alongside, because the failure mode is precisely that a
    // working corpus makes the overall figure look fine.
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

    // The control, and the reason this test is not vacuous: a genuinely empty
    // corpus must NOT raise the same flag, or the flag means nothing.
    let context = corpus_row(&body, "context");
    assert_eq!(context["rows"], 0);
    assert_eq!(context["source_rows"], 0);
    assert_eq!(
        context["structurally_excluded"], false,
        "an empty corpus is waiting for data and is not broken: {body}"
    );

    // And the shape that made this invisible: the pond still reads fully covered.
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

    // The index really is empty: the live memory now reads as missing, which is
    // what the maintenance sweep looks for. Nothing was lost -- the source row
    // is still there, which is the whole reason this file is deletable.
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
