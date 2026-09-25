//! Memory segments, tiers, decay and cleanup against a migrated tempdir SQLite; no LLM.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
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
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_memory::SqliteMemoryRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
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
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));
    let memory_repo = Arc::new(SqliteMemoryRepository::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
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
        memory_repo: memory_repo.clone(),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
        account_sync: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
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
    (router, memory_repo, tmp)
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
