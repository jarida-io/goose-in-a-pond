//! Memory segments, tiers, decay and cleanup against a migrated tempdir SQLite; no LLM.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::context::index_maintenance::{run_index_maintenance, IndexBudget};
use pond_core::context::vector_index::{Corpus, VectorIndex};
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::memory::{
    MemoryFragment, MemoryLifecycle, MemorySegment, MemoryTier,
};
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::schedule_execution::ScheduleExecutor;
use pond_core::user_data::ports::scheduler::SchedulerPort;
use pond_core::user_data::services::memory_cleanup::effective_score;
use pond_core::user_data::services::memory_relevance::content_tokens;
use pond_core::user_data::services::redacting_memory_repository::RedactingMemoryRepository;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::rule_redactor::RuleRedactor;
use pond_infra::sqlite_memory::SqliteMemoryRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_vector_index::SqliteVectorIndex;
use reqwest::Client as ReqwestClient;
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;

// ── Stubs ──────────────────────────────────────────────────────────────────────

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
    // Answering "not onboarded" would make every onboarding write route public.
    async fn is_complete(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

struct NoDevices;
#[async_trait::async_trait]
impl DeviceRegistry for NoDevices {
    async fn register(&self, req: RegisterDeviceRequest) -> anyhow::Result<Device> {
        Ok(Device {
            id: "mock".into(),
            name: req.name,
            device_type: req.device_type,
            hostname: req.hostname,
            ip_address: None,
            capabilities: req.capabilities,
            registered_at: "2024-01-01T00:00:00Z".into(),
            last_seen: None,
            is_online: false,
            room: req.room,
        })
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

// ── Helper: create a real DB-backed app with SqliteMemoryRepository ──────────

async fn make_app_with_real_memory(
) -> (axum::Router, Arc<SqliteMemoryRepository>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let memory_repo = Arc::new(SqliteMemoryRepository::new(db.system.clone()));
    let router = router_over(db, memory_repo.clone(), None, None).await;
    (router, memory_repo, tmp)
}

async fn router_over(
    db: Database,
    memory_repo: Arc<dyn MemoryRepository + Send + Sync>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    vector_index: Option<Arc<dyn VectorIndex>>,
) -> axum::Router {
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        suggestion_queue: std::sync::Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage,
        http_client: ReqwestClient::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        matter: None,
        memory_repo,
        embedding_provider,
        vector_index,
        index_reindex: None,
        lane: None,
        account_sync: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        prompt_template_dir: None,
        model_repo: None,
        data_dir: None,
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
        download_tracker: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
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
        face_recognition: None,
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
        voice_samples: Default::default(),
    });

    build_router(state, std::path::PathBuf::from("pond-desktop/dist"))
}

/// The store as `run_server` wires it: redactor outermost, index write-through inside it.
struct SemanticApp {
    router: axum::Router,
    repo: Arc<dyn MemoryRepository + Send + Sync>,
    index: Arc<dyn VectorIndex>,
    sessions: SqliteSessionStorage,
    _tmp: tempfile::TempDir,
}

async fn make_app_with_semantic_memory(
    embedder: Arc<dyn EmbeddingProvider + Send + Sync>,
) -> SemanticApp {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));
    let repo: Arc<dyn MemoryRepository + Send + Sync> = Arc::new(RedactingMemoryRepository::new(
        Arc::new(
            SqliteMemoryRepository::new(db.system.clone())
                .with_vector_index(index.clone(), Some(embedder.model_id())),
        ),
        Arc::new(RuleRedactor::new()),
    ));
    let sessions = SqliteSessionStorage::new(db.system.clone());
    let router = router_over(db, repo.clone(), Some(embedder), Some(index.clone())).await;
    SemanticApp {
        router,
        repo,
        index,
        sessions,
        _tmp: tmp,
    }
}

/// Counts shared content words and nothing else, so a top hit can only be earned by overlap.
struct WordsEmbedder;

const WORDS_DIMS: usize = 1024;

#[async_trait::async_trait]
impl EmbeddingProvider for WordsEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut v = vec![0.0_f32; WORDS_DIMS];
        for token in content_tokens(text) {
            // FNV-1a: stable across runs, unlike `DefaultHasher`.
            let hash = token.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
            });
            v[(hash % WORDS_DIMS as u64) as usize] += 1.0;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
        Ok(v)
    }

    fn dimensions(&self) -> usize {
        WORDS_DIMS
    }

    fn model_id(&self) -> String {
        "words-test".to_string()
    }
}

/// Same model id as [`WordsEmbedder`], as a provider still downloading its model would have.
struct DownEmbedder;

#[async_trait::async_trait]
impl EmbeddingProvider for DownEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        anyhow::bail!("embedding model not found; it is still downloading")
    }

    fn dimensions(&self) -> usize {
        WORDS_DIMS
    }

    fn model_id(&self) -> String {
        "words-test".to_string()
    }
}

async fn post_memory(app: &SemanticApp, segment: &str, content: &str) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(auth_post(
            "/api/v1/memories",
            serde_json::json!({"content": content, "segment": segment}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "saving {content:?}");
    json_body(resp).await["id"].as_str().unwrap().to_string()
}

async fn stored(app: &SemanticApp, id: &str) -> MemoryFragment {
    app.repo
        .search_recent(&ProfileScope::Household, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.id == id)
        .unwrap_or_else(|| panic!("memory {id} is not in the store"))
}

fn auth_get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap()
}

fn auth_post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("Authorization", "Bearer test-token")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn auth_put(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("content-type", "application/json")
        .header("Authorization", "Bearer test-token")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn auth_delete(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap()
}

async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ── Direct DB Tests (no HTTP, just the repository) ───────────────────────────

#[tokio::test]
async fn segment_fields_round_trip_through_sqlite() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let repo = SqliteMemoryRepository::new(db.system);

    let frag = MemoryFragment::from_extraction(
        "seg-1".into(),
        None,
        "User's name is Jerry".into(),
        MemorySegment::Identity,
        0.85,
        None,
    );
    repo.add(frag).await.unwrap();

    let results = repo
        .search_recent(&ProfileScope::Household, 10)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    let m = &results[0];
    assert_eq!(m.segment, Some(MemorySegment::Identity));
    assert!((m.importance.unwrap() - 0.85).abs() < 0.01);
    assert_eq!(m.tier, Some(MemoryTier::Permanent));
    assert_eq!(m.decay_rate, Some(0.0));
    assert_eq!(m.lifecycle, Some(MemoryLifecycle::Active));
    assert_eq!(m.access_count, 0);
}

#[tokio::test]
async fn access_tracking_increments_and_timestamps() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let repo = SqliteMemoryRepository::new(db.system);

    let frag = MemoryFragment::from_extraction(
        "acc-1".into(),
        None,
        "Test fact".into(),
        MemorySegment::Knowledge,
        0.5,
        None,
    );
    repo.add(frag).await.unwrap();

    repo.record_access("acc-1").await.unwrap();
    repo.record_access("acc-1").await.unwrap();
    repo.record_access("acc-1").await.unwrap();

    let results = repo
        .search_recent(&ProfileScope::Household, 10)
        .await
        .unwrap();
    assert_eq!(results[0].access_count, 3);
    assert!(results[0].last_accessed_at.is_some());
}

#[tokio::test]
async fn lifecycle_update_hides_from_search() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let repo = SqliteMemoryRepository::new(db.system);

    repo.add(MemoryFragment::from_extraction(
        "lc-1".into(),
        None,
        "Active memory".into(),
        MemorySegment::Knowledge,
        0.5,
        None,
    ))
    .await
    .unwrap();

    repo.add(MemoryFragment::from_extraction(
        "lc-2".into(),
        None,
        "To be archived".into(),
        MemorySegment::Context,
        0.3,
        None,
    ))
    .await
    .unwrap();

    repo.update_lifecycle("lc-2", MemoryLifecycle::Archived)
        .await
        .unwrap();

    let results = repo
        .search_recent(&ProfileScope::Household, 10)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "lc-1");
}

#[tokio::test]
async fn search_by_segment_filters_correctly() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let repo = SqliteMemoryRepository::new(db.system);

    repo.add(MemoryFragment::from_extraction(
        "s1".into(),
        None,
        "Name is Jerry".into(),
        MemorySegment::Identity,
        0.85,
        None,
    ))
    .await
    .unwrap();
    repo.add(MemoryFragment::from_extraction(
        "s2".into(),
        None,
        "Likes dark mode".into(),
        MemorySegment::Preference,
        0.7,
        None,
    ))
    .await
    .unwrap();
    repo.add(MemoryFragment::from_extraction(
        "s3".into(),
        None,
        "Lives in Nairobi".into(),
        MemorySegment::Identity,
        0.8,
        None,
    ))
    .await
    .unwrap();

    let identities = repo
        .search_by_segment(MemorySegment::Identity, &ProfileScope::Household, 10)
        .await
        .unwrap();
    assert_eq!(identities.len(), 2);
    assert!(identities
        .iter()
        .all(|m| m.segment == Some(MemorySegment::Identity)));

    let prefs = repo
        .search_by_segment(MemorySegment::Preference, &ProfileScope::Household, 10)
        .await
        .unwrap();
    assert_eq!(prefs.len(), 1);
}

#[tokio::test]
async fn mark_superseded_sets_lifecycle_and_link() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let repo = SqliteMemoryRepository::new(db.system);

    repo.add(MemoryFragment::from_extraction(
        "old-1".into(),
        None,
        "User likes coffee".into(),
        MemorySegment::Preference,
        0.7,
        None,
    ))
    .await
    .unwrap();
    repo.add(MemoryFragment::from_extraction(
        "new-1".into(),
        None,
        "User likes coffee and tea".into(),
        MemorySegment::Preference,
        0.7,
        None,
    ))
    .await
    .unwrap();

    repo.mark_superseded("old-1", "new-1").await.unwrap();

    // Old memory should be hidden from search (lifecycle=merged)
    let results = repo
        .search_recent(&ProfileScope::Household, 10)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "new-1");
}

// ── Effective Score Tests ────────────────────────────────────────────────────

#[test]
fn effective_score_permanent_tier_no_decay() {
    let frag = MemoryFragment::from_extraction(
        "p1".into(),
        None,
        "Name".into(),
        MemorySegment::Identity,
        0.9,
        None,
    );
    let score = effective_score(&frag, 11.25, 0.8);
    assert!((score - 0.9).abs() < 0.01);
}

#[test]
fn effective_score_decays_over_time() {
    let mut frag = MemoryFragment::from_extraction(
        "d1".into(),
        None,
        "Old fact".into(),
        MemorySegment::Context,
        0.3,
        None,
    );
    frag.created_at = chrono::Utc::now() - chrono::Duration::days(30);
    frag.tier = Some(MemoryTier::Short);
    frag.decay_rate = Some(0.1);

    let score = effective_score(&frag, 11.25, 0.8);
    assert!(
        score < 0.15,
        "30-day-old short-tier memory should decay significantly, got {score}"
    );
}

#[test]
fn effective_score_access_reinforces() {
    let mut no_access = MemoryFragment::from_extraction(
        "a1".into(),
        None,
        "Fact".into(),
        MemorySegment::Knowledge,
        0.5,
        None,
    );
    no_access.created_at = chrono::Utc::now() - chrono::Duration::days(10);

    let mut accessed = no_access.clone();
    accessed.access_count = 20;

    assert!(effective_score(&accessed, 11.25, 0.8) > effective_score(&no_access, 11.25, 0.8));
}

// ── Cleanup Integration Test ─────────────────────────────────────────────────

#[tokio::test]
async fn cleanup_archives_decayed_memories() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let repo = SqliteMemoryRepository::new(db.system);

    // Fresh memory — should survive
    repo.add(MemoryFragment::from_extraction(
        "fresh".into(),
        None,
        "Recent fact".into(),
        MemorySegment::Knowledge,
        0.7,
        None,
    ))
    .await
    .unwrap();

    // Old short-tier memory — should be pruned
    let mut old = MemoryFragment::from_extraction(
        "old".into(),
        None,
        "Ancient context".into(),
        MemorySegment::Context,
        0.3,
        None,
    );
    old.created_at = chrono::Utc::now() - chrono::Duration::days(60);
    old.tier = Some(MemoryTier::Short);
    old.decay_rate = Some(0.1);
    repo.add(old).await.unwrap();

    // Permanent memory — should always survive
    repo.add(MemoryFragment::from_extraction(
        "perm".into(),
        None,
        "Identity fact".into(),
        MemorySegment::Identity,
        0.9,
        None,
    ))
    .await
    .unwrap();

    let (scanned, archived, pruned) =
        pond_core::user_data::services::memory_cleanup::run_cleanup(&repo, 0.05, 0.15, 11.25, 0.8)
            .await
            .unwrap();

    assert!(scanned >= 2, "should scan at least 2 scoreable memories");
    assert!(
        archived + pruned >= 1,
        "should archive/prune the decayed memory"
    );

    // Verify: only fresh + perm remain in active search
    let active = repo
        .search_recent(&ProfileScope::Household, 10)
        .await
        .unwrap();
    assert_eq!(active.len(), 2);
    let ids: Vec<&str> = active.iter().map(|m| m.id.as_str()).collect();
    assert!(ids.contains(&"fresh"));
    assert!(ids.contains(&"perm"));
}

// ── REST API Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn api_save_memory_with_segment() {
    let (app, repo, _tmp) = make_app_with_real_memory().await;

    let body = serde_json::json!({
        "content": "User prefers dark mode",
        "source": "api",
        "tags": ["preferences"]
    });
    let resp = app
        .clone()
        .oneshot(auth_post("/api/v1/memories", body))
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "save memory should succeed, got {}",
        resp.status()
    );

    let memories = repo
        .search_recent(&ProfileScope::Household, 10)
        .await
        .unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].content, "User prefers dark mode");
}

#[tokio::test]
async fn api_list_memories_returns_recent() {
    let (app, repo, _tmp) = make_app_with_real_memory().await;

    repo.add(MemoryFragment::from_extraction(
        "m1".into(),
        None,
        "Fact one".into(),
        MemorySegment::Knowledge,
        0.5,
        None,
    ))
    .await
    .unwrap();
    repo.add(MemoryFragment::from_extraction(
        "m2".into(),
        None,
        "Fact two".into(),
        MemorySegment::Preference,
        0.7,
        None,
    ))
    .await
    .unwrap();

    let resp = app.oneshot(auth_get("/api/v1/memories")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = json_body(resp).await;
    let memories = json.as_array().unwrap();
    assert_eq!(memories.len(), 2);
}

#[tokio::test]
async fn api_delete_memory_removes_from_db() {
    let (app, repo, _tmp) = make_app_with_real_memory().await;

    repo.add(MemoryFragment::from_extraction(
        "del-1".into(),
        None,
        "To delete".into(),
        MemorySegment::Context,
        0.3,
        None,
    ))
    .await
    .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_delete("/api/v1/memories/del-1"))
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "delete memory should succeed, got {}",
        resp.status()
    );

    let remaining = repo
        .search_recent(&ProfileScope::Household, 10)
        .await
        .unwrap();
    assert!(remaining.is_empty());
}

async fn add_routine(repo: &SqliteMemoryRepository, id: &str) {
    repo.add(MemoryFragment::from_extraction(
        id.into(),
        None,
        "Orders sushi on Fridays".into(),
        MemorySegment::Routine,
        0.65,
        None,
    ))
    .await
    .unwrap();
}

async fn only_memory(repo: &SqliteMemoryRepository) -> MemoryFragment {
    let mut all = repo
        .search_recent(&ProfileScope::Household, 10)
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    all.remove(0)
}

#[tokio::test]
async fn api_update_memory_keeps_it_without_touching_the_text() {
    let (app, repo, _tmp) = make_app_with_real_memory().await;
    add_routine(&repo, "put-1").await;

    let resp = app
        .clone()
        .oneshot(auth_put(
            "/api/v1/memories/put-1",
            serde_json::json!({ "tier": "permanent" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let kept = only_memory(&repo).await;
    assert_eq!(kept.tier, Some(MemoryTier::Permanent));
    assert_eq!(kept.decay_rate, Some(0.0));
    assert_eq!(kept.content, "Orders sushi on Fridays");
}

#[tokio::test]
async fn api_update_memory_rewords_and_retiers_together() {
    let (app, repo, _tmp) = make_app_with_real_memory().await;
    add_routine(&repo, "put-2").await;

    let resp = app
        .clone()
        .oneshot(auth_put(
            "/api/v1/memories/put-2",
            serde_json::json!({ "content": "  Orders Thai on Fridays ", "tier": "short" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let updated = only_memory(&repo).await;
    assert_eq!(updated.content, "Orders Thai on Fridays");
    assert_eq!(updated.tier, Some(MemoryTier::Short));
}

#[tokio::test]
async fn api_update_memory_rewording_alone_leaves_the_tier() {
    let (app, repo, _tmp) = make_app_with_real_memory().await;
    add_routine(&repo, "put-3").await;

    let resp = app
        .clone()
        .oneshot(auth_put(
            "/api/v1/memories/put-3",
            serde_json::json!({ "content": "Orders Thai on Fridays" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let updated = only_memory(&repo).await;
    assert_eq!(updated.content, "Orders Thai on Fridays");
    assert_eq!(updated.tier, Some(MemoryTier::Long));
}

#[tokio::test]
async fn api_update_memory_refuses_an_empty_or_blank_change() {
    let (app, repo, _tmp) = make_app_with_real_memory().await;
    add_routine(&repo, "put-4").await;

    for body in [
        serde_json::json!({}),
        serde_json::json!({ "content": "   " }),
        serde_json::json!({ "content": "", "tier": "permanent" }),
    ] {
        let resp = app
            .clone()
            .oneshot(auth_put("/api/v1/memories/put-4", body.clone()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{body}");
    }

    let untouched = only_memory(&repo).await;
    assert_eq!(untouched.content, "Orders sushi on Fridays");
    assert_eq!(untouched.tier, Some(MemoryTier::Long));
}

#[tokio::test]
async fn api_update_memory_refuses_an_unknown_tier() {
    let (app, repo, _tmp) = make_app_with_real_memory().await;
    add_routine(&repo, "put-5").await;

    let resp = app
        .clone()
        .oneshot(auth_put(
            "/api/v1/memories/put-5",
            serde_json::json!({ "tier": "forever" }),
        ))
        .await
        .unwrap();
    assert!(resp.status().is_client_error(), "got {}", resp.status());
    assert_eq!(only_memory(&repo).await.tier, Some(MemoryTier::Long));
}

// ── Semantic recall of memories written through the API ─────────────────────

/// Both reads semantic recall makes: the turn's injection scores `memory_fragments.embedding`,
/// and the `recall` tool searches the shared index.
#[tokio::test]
async fn a_memory_saved_through_the_route_is_returned_by_semantic_search() {
    let app = make_app_with_semantic_memory(Arc::new(WordsEmbedder)).await;

    // Recency and importance alone would never put the project memory first.
    let mut project = String::new();
    for (segment, content) in [
        ("identity", "My name is Jerry and I live in Nairobi"),
        ("preference", "I prefer tea to coffee in the morning"),
        (
            "relationship",
            "Wanjiru is my sister and she lives in Mombasa",
        ),
        (
            "project",
            "I am building a solar irrigation controller for the greenhouse",
        ),
        ("routine", "I go running every Saturday at six"),
        (
            "preference",
            "I like the living room lights dimmed after nine",
        ),
    ] {
        let id = post_memory(&app, segment, content).await;
        if segment == "project" {
            project = id;
        }
    }

    let query = WordsEmbedder
        .embed_query("What am I building at the moment?")
        .await
        .unwrap();

    let hits = app
        .repo
        .search_similar(&query, &ProfileScope::Household, 6)
        .await
        .unwrap();
    assert_eq!(
        hits.first().map(|m| m.content.as_str()),
        Some("I am building a solar irrigation controller for the greenhouse"),
        "semantic search did not put the project memory first: {:?}",
        hits.iter().map(|m| &m.content).collect::<Vec<_>>()
    );
    assert!(
        hits[0].embedding.is_some(),
        "without a vector the turn scores it on importance and recency only"
    );

    let resolved = app
        .index
        .search_resolved(&query, "words-test", &ProfileScope::Household, 3)
        .await
        .unwrap();
    assert_eq!(
        resolved.first().map(|h| h.row_id.as_str()),
        Some(project.as_str()),
        "the shared index does not hold the memory: {resolved:?}"
    );

    assert!(
        app.repo.search_unembedded(10).await.unwrap().is_empty(),
        "a memory saved through the route was left for the sweep"
    );
}

#[tokio::test]
async fn rewording_a_memory_through_the_route_re_embeds_it() {
    let app = make_app_with_semantic_memory(Arc::new(WordsEmbedder)).await;
    let id = post_memory(
        &app,
        "project",
        "I am building a solar irrigation controller",
    )
    .await;

    let reworded = "I am restoring a vintage motorbike";
    let resp = app
        .router
        .clone()
        .oneshot(auth_put(
            &format!("/api/v1/memories/{id}"),
            serde_json::json!({ "content": reworded }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let expected = WordsEmbedder.embed(reworded).await.unwrap();
    let row = stored(&app, &id).await;
    assert_eq!(row.content, reworded);
    assert_eq!(
        row.embedding.as_ref(),
        Some(&expected),
        "the row's vector is not the new words'"
    );
    let entry = app.index.get(Corpus::Memory, &id).await.unwrap();
    assert_eq!(
        entry.map(|e| e.vector),
        Some(expected),
        "the index still scores the old words"
    );
}

#[tokio::test]
async fn re_tiering_a_memory_through_the_route_keeps_its_vector() {
    let app = make_app_with_semantic_memory(Arc::new(WordsEmbedder)).await;
    let content = "I am building a solar irrigation controller";
    let id = post_memory(&app, "project", content).await;

    let resp = app
        .router
        .clone()
        .oneshot(auth_put(
            &format!("/api/v1/memories/{id}"),
            serde_json::json!({ "tier": "permanent" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let expected = WordsEmbedder.embed(content).await.unwrap();
    let row = stored(&app, &id).await;
    assert_eq!(row.tier, Some(MemoryTier::Permanent));
    assert_eq!(
        row.embedding.as_ref(),
        Some(&expected),
        "a tier change dropped the row's vector"
    );
    let entry = app.index.get(Corpus::Memory, &id).await.unwrap();
    assert_eq!(
        entry.map(|e| e.vector),
        Some(expected),
        "a tier change dropped the index entry"
    );
}

#[tokio::test]
async fn a_credential_never_reaches_a_vector_through_either_memory_route() {
    const KEY: &str = "sk-abcdefghijklmnopqrstuvwxyz123456";
    let app = make_app_with_semantic_memory(Arc::new(WordsEmbedder)).await;

    let saved = post_memory(&app, "knowledge", &format!("the stripe key is {KEY}")).await;
    let row = stored(&app, &saved).await;
    assert!(
        row.content.contains("[redacted:api-key]"),
        "{}",
        row.content
    );
    assert!(
        row.embedding.is_none(),
        "a vector of the unredacted text was stored"
    );
    assert!(
        app.index
            .get(Corpus::Memory, &saved)
            .await
            .unwrap()
            .is_none(),
        "a vector of the unredacted text reached the index"
    );

    let edited = post_memory(&app, "knowledge", "the spare key is under the mat").await;
    assert!(
        app.index
            .get(Corpus::Memory, &edited)
            .await
            .unwrap()
            .is_some(),
        "the ordinary memory was never indexed, so the check below proves nothing"
    );
    let resp = app
        .router
        .clone()
        .oneshot(auth_put(
            &format!("/api/v1/memories/{edited}"),
            serde_json::json!({ "content": format!("the new stripe key is {KEY}") }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let row = stored(&app, &edited).await;
    assert!(!row.content.contains(KEY), "{}", row.content);
    assert!(
        row.embedding.is_none(),
        "an edit stored a vector of the unredacted text"
    );
    assert!(
        app.index
            .get(Corpus::Memory, &edited)
            .await
            .unwrap()
            .is_none(),
        "the old words' entry outlived the edit"
    );
}

/// The save never waits on a working embedder; a scheduled sweep tick embeds the row later.
#[tokio::test]
async fn a_memory_saved_while_the_embedder_is_down_is_kept_and_a_sweep_embeds_it() {
    let app = make_app_with_semantic_memory(Arc::new(DownEmbedder)).await;
    let id = post_memory(
        &app,
        "project",
        "I am building a solar irrigation controller for the greenhouse",
    )
    .await;

    let waiting: Vec<String> = app
        .repo
        .search_unembedded(10)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(waiting, vec![id.clone()], "the sweep cannot see the row");

    let report = run_index_maintenance(
        &app.index,
        &app.sessions,
        app.repo.as_ref(),
        &WordsEmbedder,
        &tokio_util::sync::CancellationToken::new(),
        IndexBudget::OneBatch,
    )
    .await;
    assert_eq!(report.memories_indexed, 1);

    let query = WordsEmbedder
        .embed_query("What am I building at the moment?")
        .await
        .unwrap();
    let resolved = app
        .index
        .search_resolved(&query, "words-test", &ProfileScope::Household, 1)
        .await
        .unwrap();
    assert_eq!(
        resolved.first().map(|h| h.row_id.as_str()),
        Some(id.as_str())
    );
}

// ── Token Usage Summary API Test ─────────────────────────────────────────────

#[tokio::test]
async fn api_usage_summary_returns_zeros_initially() {
    let (app, _repo, _tmp) = make_app_with_real_memory().await;

    let resp = app
        .oneshot(auth_get("/api/v1/usage/summary"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = json_body(resp).await;
    assert_eq!(json["total_tokens"].as_u64(), Some(0));
    assert_eq!(json["session_count"].as_u64(), Some(0));
    // Pricing should come from settings defaults
    assert!(json["cloud_input_price_per_million"].as_f64().is_some());
    assert!(json["cloud_output_price_per_million"].as_f64().is_some());
}
