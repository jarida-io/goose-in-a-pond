//! A stored reminder can be seen and dismissed over HTTP, against a real database.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::reminder::{CapturedReminder, ReminderDisposition};
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::reminder_repository::ReminderRepository;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_reminder::SqliteReminderRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use serde_json::Value;
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

struct Harness {
    app: axum::Router,
    reminders: SqliteReminderRepository,
    profiles: Arc<SqliteProfileRepository>,
    storage: Arc<SqliteSessionStorage>,
    _tmp: tempfile::TempDir,
}

async fn make_app() -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let storage = Arc::new(SqliteSessionStorage::new(pool.clone()));
    let profiles = Arc::new(SqliteProfileRepository::new(pool.clone()));
    let hs = MockHandshake::new();
    hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        suggestion_queue: std::sync::Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        transcribe_audio: None,
        session_storage: storage.clone(),
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
        setup_claims: None,
        spoken_codes: Default::default(),
        voice_samples: Default::default(),
    });

    Harness {
        app: build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        reminders: SqliteReminderRepository::new(pool),
        profiles,
        storage,
        _tmp: tmp,
    }
}

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// One reminder as extraction writes it: no `profile_id`, as on every live pond.
fn reminder(id: &str, about: &str, said_ago: Duration) -> CapturedReminder {
    CapturedReminder {
        id: id.into(),
        about: about.into(),
        when_said: "next Tuesday".into(),
        session_id: "sess-1".into(),
        window_id: format!("win-{id}"),
        subject: "Jerry".into(),
        profile_id: None,
        said_at: Utc::now() - said_ago,
        captured_at: Utc::now(),
        disposition: ReminderDisposition::Pending,
    }
}

// ── HTTP helpers ─────────────────────────────────────────────────────────────

async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    send(app, "GET", uri).await
}

async fn post_json(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    send(app, "POST", uri).await
}

async fn send(app: &axum::Router, method: &str, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
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

fn ids(body: &Value) -> Vec<String> {
    body["reminders"]
        .as_array()
        .unwrap_or_else(|| panic!("no `reminders` array in {body}"))
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

// ── The read ─────────────────────────────────────────────────────────────────

/// Every live row is unowned, so unlike `/proposals` this route needs no audience.
#[tokio::test]
async fn a_reminder_nobody_owns_is_still_readable() {
    let h = make_app().await;
    h.reminders
        .capture(&reminder("r-1", "the dentist", Duration::hours(2)))
        .await
        .unwrap();

    let (status, body) = get_json(&h.app, "/api/v1/reminders").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&body), vec!["r-1"]);

    let row = &body["reminders"][0];
    assert_eq!(row["about"], "the dentist");
    assert_eq!(
        row["when_said"], "next Tuesday",
        "the subject's own words, not a date"
    );
    assert_eq!(row["profile_id"], Value::Null);
    assert_eq!(row["disposition"], "pending");
    assert_eq!(
        row["session_id"], "sess-1",
        "a reminder must be able to say where it came from"
    );
    assert!(
        row.get("due_at").is_none(),
        "there is no resolved date anywhere in this design"
    );
}

/// Ordered by when it was said: a backlog run captures a year of history in one night.
#[tokio::test]
async fn the_most_recently_said_comes_first() {
    let h = make_app().await;
    h.reminders
        .capture(&reminder("old", "the school run", Duration::days(5)))
        .await
        .unwrap();
    h.reminders
        .capture(&reminder("new", "the dentist", Duration::hours(1)))
        .await
        .unwrap();

    let (_, body) = get_json(&h.app, "/api/v1/reminders").await;
    assert_eq!(ids(&body), vec!["new", "old"]);
}

// ── The disposition action ───────────────────────────────────────────────────

#[tokio::test]
async fn dismissing_takes_it_off_the_list_and_only_once() {
    let h = make_app().await;
    h.reminders
        .capture(&reminder("r-1", "the dentist", Duration::hours(2)))
        .await
        .unwrap();

    let (status, body) = post_json(&h.app, "/api/v1/reminders/r-1/dismiss").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["disposition"], "dismissed");

    let (_, listed) = get_json(&h.app, "/api/v1/reminders").await;
    assert!(ids(&listed).is_empty(), "a decision is not asked again");

    // 404, not a second success: the pond must not claim a dismissal it didn't make.
    let (status, _) = post_json(&h.app, "/api/v1/reminders/r-1/dismiss").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn dismissing_something_that_was_never_there_is_a_404() {
    let h = make_app().await;
    let (status, body) = post_json(&h.app, "/api/v1/reminders/nobody/dismiss").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body["error"].as_str().unwrap_or_default().len() > 10,
        "and it says so in words"
    );
}

// ── Routing ──────────────────────────────────────────────────────────────────

/// Via the built router, since only registration order in `routes.rs` decides the match.
#[tokio::test]
async fn a_literal_segment_is_not_matched_as_an_id() {
    let h = make_app().await;

    let (status, body) = get_json(&h.app, "/api/v1/memories/extraction-status").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("running").is_some(),
        "this is the status object, not a memory called `extraction-status`: {body}"
    );

    // `/reminders` resolves to its own handler, not to anything under `/memories`.
    let (status, body) = get_json(&h.app, "/api/v1/reminders").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("reminders").is_some(), "{body}");
}

/// The echoed `limit` lets a caller tell an exactly-full page from the end of the list.
#[tokio::test]
async fn the_page_is_bounded_and_says_what_it_was_asked_for() {
    let h = make_app().await;
    for i in 0..3 {
        h.reminders
            .capture(&reminder(
                &format!("r-{i}"),
                &format!("thing {i}"),
                Duration::hours(i + 1),
            ))
            .await
            .unwrap();
    }

    let (_, body) = get_json(&h.app, "/api/v1/reminders?limit=2").await;
    assert_eq!(ids(&body).len(), 2);
    assert_eq!(body["limit"], 2);

    let (_, body) = get_json(&h.app, "/api/v1/reminders?limit=99999").await;
    assert_eq!(body["limit"], 500, "clamped, not honoured");
}

async fn a_member(h: &Harness, name: &str) -> String {
    use pond_core::user_data::domain::profile::CreateProfileRequest;
    use pond_core::user_data::ports::profile::ProfileRepository;
    h.profiles
        .create(CreateProfileRequest {
            display_name: name.to_string(),
            avatar_emoji: "\u{1F986}".to_string(),
        })
        .await
        .unwrap()
        .id
}

async fn a_session_of(h: &Harness, id: &str, profile_id: &str) -> String {
    use pond_core::user_data::domain::session::{IdentificationSource, SessionIdentity};
    use pond_core::user_data::ports::session_storage::SessionStorage;
    h.storage.create_session(id.to_string()).await.unwrap();
    h.storage
        .set_session_identity(
            id,
            &SessionIdentity {
                profile_id: Some(profile_id.to_string()),
                source: IdentificationSource::Explicit,
                confidence: None,
            },
        )
        .await
        .unwrap();
    id.to_string()
}

/// The route must pass the caller's scope, not `Household`; the adapter tests cover the SQL.
#[tokio::test]
async fn a_members_reminder_reaches_that_member_and_nobody_else() {
    let h = make_app().await;
    let jerry = a_member(&h, "Jerry").await;
    let liz = a_member(&h, "Liz").await;
    let mut lizs = reminder("r-liz", "the clinic", Duration::hours(1));
    lizs.profile_id = Some(liz.clone());
    assert!(h.reminders.capture(&lizs).await.unwrap());

    let (status, body) = get_json(&h.app, "/api/v1/reminders").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        !ids(&body).contains(&"r-liz".to_string()),
        "a guest read Liz's reminder: {body}"
    );

    let s_jerry = a_session_of(&h, "s-jerry", &jerry).await;
    let (_, body) = get_json(&h.app, &format!("/api/v1/reminders?session_id={s_jerry}")).await;
    assert!(
        !ids(&body).contains(&"r-liz".to_string()),
        "Jerry read Liz's reminder: {body}"
    );

    // A guest's dismiss gets the same 404 as a missing id, so the refusal discloses nothing.
    let (status, _) = post_json(&h.app, "/api/v1/reminders/r-liz/dismiss").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a guest dismissed Liz's reminder"
    );

    // The control: Liz sees it, still pending after the refused dismiss.
    let s_liz = a_session_of(&h, "s-liz", &liz).await;
    let (_, body) = get_json(&h.app, &format!("/api/v1/reminders?session_id={s_liz}")).await;
    assert!(
        ids(&body).contains(&"r-liz".to_string()),
        "Liz could not read her own reminder: {body}"
    );
}
