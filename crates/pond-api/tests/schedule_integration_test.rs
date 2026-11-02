//! Scheduler REST routes under `/api/v1/schedules`, against an in-memory scheduler.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::schedule::{
    MissedRun, MissedRunAnswer, NoMissedRun, OneShotTimePassed, Schedule, ScheduleRun, TaskKind,
    CRON_ONCE,
};
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::schedule_execution::ScheduleExecutor;
use pond_core::user_data::ports::scheduler::{
    CreateScheduleRequest, SchedulerPort, UpdateScheduleRequest,
};
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use reqwest::Client as ReqwestClient;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
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
            id: "mock".to_string(),
            name: req.name,
            device_type: req.device_type,
            hostname: req.hostname,
            ip_address: None,
            capabilities: req.capabilities,
            registered_at: "2024-01-01T00:00:00Z".to_string(),
            last_seen: None,
            is_online: false,
            room_id: req.room_id,
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

// ── In-memory mock scheduler ──────────────────────────────────────────────────

struct InMemoryScheduler {
    tasks: Mutex<HashMap<String, Schedule>>,
}

impl InMemoryScheduler {
    fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl SchedulerPort for InMemoryScheduler {
    async fn create_task(&self, req: CreateScheduleRequest) -> anyhow::Result<Schedule> {
        let mut guard = self.tasks.lock().await;
        if guard.contains_key(&req.id) {
            anyhow::bail!("task '{}' already exists", req.id);
        }
        // As the real adapter stores it: a `fire_at` makes a one-shot with the sentinel cron.
        let cron = if req.fire_at.is_some() {
            CRON_ONCE.to_string()
        } else {
            req.cron
        };
        let schedule = Schedule {
            ask_if_missed: req.ask_if_missed,
            missed_run: None,
            fire_at: req.fire_at,
            id: req.id.clone(),
            label: req.label,
            cron,
            timezone: req.timezone,
            kind: req.kind,
            last_run: None,
            next_run: None,
            paused: false,
            currently_running: false,
            created_at: chrono::Utc::now(),
        };
        guard.insert(req.id, schedule.clone());
        Ok(schedule)
    }

    async fn list_tasks(&self) -> anyhow::Result<Vec<Schedule>> {
        Ok(self.tasks.lock().await.values().cloned().collect())
    }

    async fn delete_task(&self, id: &str) -> anyhow::Result<()> {
        let mut guard = self.tasks.lock().await;
        guard
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| anyhow::anyhow!("not found"))
    }

    async fn pause_task(&self, id: &str) -> anyhow::Result<()> {
        let mut guard = self.tasks.lock().await;
        guard
            .get_mut(id)
            .map(|t| t.paused = true)
            .ok_or_else(|| anyhow::anyhow!("not found"))
    }

    /// Mirrors the port contract: a paused one-shot whose time has passed is refused.
    async fn resume_task(&self, id: &str) -> anyhow::Result<()> {
        let mut guard = self.tasks.lock().await;
        let task = guard
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("not found"))?;
        if let Some(at) = task
            .fire_at
            .filter(|at| task.paused && *at <= chrono::Utc::now())
        {
            return Err(OneShotTimePassed {
                id: id.to_string(),
                fire_at: at,
            }
            .into());
        }
        task.paused = false;
        Ok(())
    }

    async fn run_now(&self, id: &str) -> anyhow::Result<()> {
        let guard = self.tasks.lock().await;
        guard
            .get(id)
            .map(|_| ())
            .ok_or_else(|| anyhow::anyhow!("not found"))
    }

    async fn update_task(&self, id: &str, req: UpdateScheduleRequest) -> anyhow::Result<Schedule> {
        let mut guard = self.tasks.lock().await;
        let task = guard
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("not found"))?;
        if let Some(label) = req.label {
            task.label = label;
        }
        if let Some(cron) = req.cron {
            task.cron = cron;
        }
        if let Some(tz) = req.timezone {
            task.timezone = tz;
        }
        if let Some(kind) = req.kind {
            task.kind = kind;
        }
        if let Some(ask) = req.ask_if_missed {
            task.ask_if_missed = ask;
        }
        if let Some(at) = req.fire_at {
            task.fire_at = Some(at);
        }
        Ok(task.clone())
    }

    async fn answer_missed_run(&self, id: &str, _answer: MissedRunAnswer) -> anyhow::Result<()> {
        let mut guard = self.tasks.lock().await;
        let task = guard
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("not found"))?;
        task.missed_run
            .take()
            .map(|_| ())
            .ok_or_else(|| NoMissedRun { id: id.to_string() }.into())
    }

    async fn get_runs(&self, _schedule_id: &str, _limit: u32) -> anyhow::Result<Vec<ScheduleRun>> {
        Ok(vec![])
    }

    async fn list_upcoming(&self, limit: u32) -> anyhow::Result<Vec<Schedule>> {
        let guard = self.tasks.lock().await;
        let mut schedules: Vec<_> = guard.values().filter(|t| !t.paused).cloned().collect();
        schedules.truncate(limit as usize);
        Ok(schedules)
    }

    async fn set_executor(&self, _: Arc<dyn ScheduleExecutor>) -> anyhow::Result<()> {
        Ok(())
    }
}

// ── App factory ───────────────────────────────────────────────────────────────

async fn make_app() -> (axum::Router, tempfile::TempDir) {
    make_app_with(Arc::new(InMemoryScheduler::new())).await
}

async fn make_app_with(scheduler: Arc<InMemoryScheduler>) -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let scheduler: Option<Arc<dyn SchedulerPort>> = Some(scheduler);

    let state = Arc::new(AppState {
        warmup: Default::default(),
        suggestion_queue: std::sync::Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
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
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        room_registry: Arc::new(pond_core::user_data::mocks::mock_room_registry::MockRoomRegistry),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
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
        scheduler,
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
        pond_admin: None,
    });
    (
        build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        tmp,
    )
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

// ── Tests ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_schedules_returns_empty_array_initially() {
    let (app, _tmp) = make_app().await;

    let resp = app.oneshot(auth_get("/api/v1/schedules")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = json_body(resp).await;
    assert!(
        json.is_array(),
        "expected array from GET /schedules, got: {json}"
    );
}

#[tokio::test]
async fn create_schedule_returns_created_task() {
    let (app, _tmp) = make_app().await;

    // Test new format with `prompt` field directly.
    let body = serde_json::json!({
        "name": "Daily Summary",
        "cron": "0 0 8 * * *",
        "prompt": "Summarize yesterday's events",
        "timezone": "Africa/Nairobi"
    });

    let resp = app
        .oneshot(auth_post("/api/v1/schedules", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = json_body(resp).await;
    // ID is auto-generated.
    assert!(json.get("id").and_then(|v| v.as_str()).is_some());
    assert_eq!(
        json.get("label").and_then(|v| v.as_str()),
        Some("Daily Summary")
    );
    assert_eq!(
        json.get("timezone").and_then(|v| v.as_str()),
        Some("Africa/Nairobi")
    );
    assert_eq!(json.get("paused").and_then(|v| v.as_bool()), Some(false));
}

#[tokio::test]
async fn create_schedule_legacy_payload_format() {
    let (app, _tmp) = make_app().await;

    // Legacy format with `payload.prompt`.
    let body = serde_json::json!({
        "id": "legacy-task",
        "name": "Legacy Task",
        "cron": "0 0 7 * * *",
        "payload": {"prompt": "What's the weather today?"}
    });

    let resp = app
        .oneshot(auth_post("/api/v1/schedules", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = json_body(resp).await;
    assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("legacy-task"));
}

#[tokio::test]
async fn created_schedule_appears_in_list() {
    let (app, _tmp) = make_app().await;

    let body = serde_json::json!({
        "id": "weather-check",
        "name": "Weather Check",
        "cron": "0 0 7 * * *",
        "prompt": "What's the weather today?"
    });
    app.clone()
        .oneshot(auth_post("/api/v1/schedules", body))
        .await
        .unwrap();

    let list_resp = app.oneshot(auth_get("/api/v1/schedules")).await.unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);

    let json = json_body(list_resp).await;
    let tasks = json.as_array().unwrap();
    assert!(
        tasks
            .iter()
            .any(|t| t.get("id").and_then(|v| v.as_str()) == Some("weather-check")),
        "created task not found in list: {tasks:?}"
    );
}

#[tokio::test]
async fn delete_schedule_removes_it() {
    let (app, _tmp) = make_app().await;

    let body = serde_json::json!({
        "id": "to-delete",
        "name": "To Delete",
        "cron": "0 0 9 * * *",
        "prompt": "delete me"
    });
    app.clone()
        .oneshot(auth_post("/api/v1/schedules", body))
        .await
        .unwrap();

    let del_resp = app
        .clone()
        .oneshot(auth_delete("/api/v1/schedules/to-delete"))
        .await
        .unwrap();
    assert_eq!(del_resp.status(), StatusCode::OK);

    let list_resp = app.oneshot(auth_get("/api/v1/schedules")).await.unwrap();
    let json = json_body(list_resp).await;
    let tasks = json.as_array().unwrap();
    assert!(
        !tasks
            .iter()
            .any(|t| t.get("id").and_then(|v| v.as_str()) == Some("to-delete")),
        "deleted task still in list: {tasks:?}"
    );
}

#[tokio::test]
async fn pause_and_resume_schedule() {
    let (app, _tmp) = make_app().await;

    let body = serde_json::json!({
        "id": "pausable",
        "name": "Pausable",
        "cron": "0 0 10 * * *",
        "prompt": "pause me"
    });
    app.clone()
        .oneshot(auth_post("/api/v1/schedules", body))
        .await
        .unwrap();

    let pause_resp = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/schedules/pausable/pause",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(pause_resp.status(), StatusCode::OK);

    let list_resp = app
        .clone()
        .oneshot(auth_get("/api/v1/schedules"))
        .await
        .unwrap();
    let json = json_body(list_resp).await;
    let task = json
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("pausable"))
        .expect("task not found");
    assert_eq!(task.get("paused").and_then(|v| v.as_bool()), Some(true));

    let resume_resp = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/schedules/pausable/resume",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resume_resp.status(), StatusCode::OK);

    let list_resp2 = app.oneshot(auth_get("/api/v1/schedules")).await.unwrap();
    let json2 = json_body(list_resp2).await;
    let task2 = json2
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("pausable"))
        .expect("task not found after resume");
    assert_eq!(task2.get("paused").and_then(|v| v.as_bool()), Some(false));
}

/// Resuming a paused one-shot whose time passed is a 409 with a stable code, not the 404 a
/// missing schedule gets: the schedule is there, and the answer says to edit its time.
#[tokio::test]
async fn resuming_a_one_shot_whose_time_passed_is_a_conflict_not_a_not_found() {
    let (app, _tmp) = make_app().await;
    app.clone()
        .oneshot(auth_post(
            "/api/v1/schedules",
            serde_json::json!({
                "id": "missed",
                "name": "Water the plants",
                "cron": "0 0 10 * * *",
                "prompt": "water the plants"
            }),
        ))
        .await
        .unwrap();
    let past = chrono::Utc::now() - chrono::Duration::hours(1);
    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/schedules/missed")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({ "fire_at": past })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let pause = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/schedules/missed/pause",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(pause.status(), StatusCode::OK);

    let resume = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/schedules/missed/resume",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resume.status(), StatusCode::CONFLICT);
    let json = json_body(resume).await;
    assert_eq!(
        json.get("code").and_then(|v| v.as_str()),
        Some("one_shot_time_passed")
    );
    assert!(
        json.get("error")
            .and_then(|v| v.as_str())
            .is_some_and(|e| e.contains("edit its time first")),
        "the error must say what to do: {json}"
    );

    let missing = app
        .oneshot(auth_post(
            "/api/v1/schedules/ghost/resume",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

/// A phone creates a one-off with `POST` and a `fire_at`, as `PUT` already allowed.
#[tokio::test]
async fn create_one_off_with_fire_at() {
    let (app, _tmp) = make_app().await;
    let at = chrono::Utc::now() + chrono::Duration::hours(2);
    let resp = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/schedules",
            serde_json::json!({
                "id": "once",
                "name": "Take the bread out",
                "prompt": "remind me to take the bread out",
                "timezone": "Africa/Nairobi",
                "fire_at": at,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = json_body(resp).await;
    assert_eq!(json.get("cron").and_then(|v| v.as_str()), Some(CRON_ONCE));
    let stored: chrono::DateTime<chrono::Utc> = json
        .get("fire_at")
        .and_then(|v| v.as_str())
        .expect("a one-off create must carry its fire_at")
        .parse()
        .unwrap();
    assert_eq!(stored, at);
}

/// A `fire_at` already past is refused with a code, not created to fire on the spot.
#[tokio::test]
async fn create_with_a_past_fire_at_is_refused() {
    let (app, _tmp) = make_app().await;
    let resp = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/schedules",
            serde_json::json!({
                "id": "late",
                "name": "Too late",
                "prompt": "too late",
                "fire_at": chrono::Utc::now() - chrono::Duration::minutes(5),
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let json = json_body(resp).await;
    assert_eq!(
        json.get("code").and_then(|v| v.as_str()),
        Some("fire_at_in_past")
    );

    let list = json_body(app.oneshot(auth_get("/api/v1/schedules")).await.unwrap()).await;
    assert_eq!(
        list.as_array().map(Vec::len),
        Some(0),
        "a refused create stored a schedule"
    );
}

/// A plain cron create is untouched by the one-off fields: no `fire_at`, its own cron.
#[tokio::test]
async fn create_with_a_cron_stays_recurring() {
    let (app, _tmp) = make_app().await;
    let resp = app
        .oneshot(auth_post(
            "/api/v1/schedules",
            serde_json::json!({
                "id": "daily",
                "name": "Daily",
                "cron": "0 0 7 * * *",
                "prompt": "good morning",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = json_body(resp).await;
    assert_eq!(
        json.get("cron").and_then(|v| v.as_str()),
        Some("0 0 7 * * *")
    );
    assert!(json.get("fire_at").is_some_and(|v| v.is_null()));
}

#[tokio::test]
async fn run_now_returns_accepted() {
    let (app, _tmp) = make_app().await;

    let body = serde_json::json!({
        "id": "run-now-task",
        "name": "Run Now",
        "cron": "0 0 11 * * *",
        "prompt": "run this now"
    });
    app.clone()
        .oneshot(auth_post("/api/v1/schedules", body))
        .await
        .unwrap();

    let run_resp = app
        .oneshot(auth_post(
            "/api/v1/schedules/run-now-task/run-now",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(run_resp.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn delete_nonexistent_schedule_returns_not_found() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .oneshot(auth_delete("/api/v1/schedules/ghost-task"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_runs_returns_empty_for_new_schedule() {
    let (app, _tmp) = make_app().await;

    let body = serde_json::json!({
        "id": "has-no-runs",
        "name": "No Runs",
        "cron": "0 0 12 * * *",
        "prompt": "test"
    });
    app.clone()
        .oneshot(auth_post("/api/v1/schedules", body))
        .await
        .unwrap();

    let resp = app
        .oneshot(auth_get("/api/v1/schedules/has-no-runs/runs"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = json_body(resp).await;
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn upcoming_returns_active_schedules() {
    let (app, _tmp) = make_app().await;

    let body = serde_json::json!({
        "id": "upcoming-task",
        "name": "Upcoming",
        "cron": "0 0 8 * * *",
        "prompt": "test"
    });
    app.clone()
        .oneshot(auth_post("/api/v1/schedules", body))
        .await
        .unwrap();

    let resp = app
        .oneshot(auth_get("/api/v1/schedules/upcoming"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = json_body(resp).await;
    let tasks = json.as_array().unwrap();
    assert!(
        tasks
            .iter()
            .any(|t| t.get("id").and_then(|v| v.as_str()) == Some("upcoming-task")),
        "upcoming task not found: {tasks:?}"
    );
}

#[tokio::test]
async fn asking_if_missed_is_set_on_create_and_changed_on_edit() {
    let (app, _tmp) = make_app().await;
    let created = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/schedules",
            serde_json::json!({
                "id": "morning",
                "name": "Morning",
                "cron": "0 30 6 * * *",
                "prompt": "good morning",
                "ask_if_missed": true,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(json_body(created).await["ask_if_missed"], true);

    let edited = app
        .clone()
        .oneshot(auth_put(
            "/api/v1/schedules/morning",
            serde_json::json!({"ask_if_missed": false}),
        ))
        .await
        .unwrap();
    assert_eq!(json_body(edited).await["ask_if_missed"], false);
}

#[tokio::test]
async fn the_first_answer_to_a_missed_run_settles_it_and_a_second_is_a_conflict() {
    let scheduler = Arc::new(InMemoryScheduler::new());
    scheduler
        .create_task(CreateScheduleRequest {
            id: "morning".into(),
            label: "Morning".into(),
            cron: "0 30 6 * * *".into(),
            fire_at: None,
            once: false,
            timezone: "UTC".into(),
            kind: TaskKind::AgentPrompt {
                prompt: "good morning".into(),
            },
            ask_if_missed: true,
        })
        .await
        .unwrap();
    scheduler
        .tasks
        .lock()
        .await
        .get_mut("morning")
        .unwrap()
        .missed_run = Some(MissedRun {
        due_at: chrono::Utc::now() - chrono::Duration::hours(3),
    });
    let (app, _tmp) = make_app_with(scheduler).await;

    let skip = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/schedules/morning/missed/skip",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(skip.status(), StatusCode::OK);

    let again = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/schedules/morning/missed/run",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::CONFLICT);
    assert_eq!(json_body(again).await["code"], "no_missed_run");

    let unknown = app
        .oneshot(auth_post(
            "/api/v1/schedules/nope/missed/run",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

fn morning_routine(devices: &[&str]) -> serde_json::Value {
    let actions: Vec<_> = devices
        .iter()
        .map(|d| serde_json::json!({"type": "device_power", "device_id": d, "on": true}))
        .collect();
    serde_json::json!({"type": "routine", "actions": actions})
}

#[tokio::test]
async fn a_routines_actions_are_created_and_replaced_through_the_api() {
    let (app, _tmp) = make_app().await;
    let created = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/schedules",
            serde_json::json!({
                "id": "morning",
                "name": "Morning",
                "cron": "0 30 6 * * *",
                "kind": morning_routine(&["kitchen-lights"]),
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);

    let edited = app
        .clone()
        .oneshot(auth_put(
            "/api/v1/schedules/morning",
            serde_json::json!({"kind": morning_routine(&["kitchen-lights", "water-heater-plug"])}),
        ))
        .await
        .unwrap();
    assert_eq!(edited.status(), StatusCode::OK);
    assert_eq!(
        json_body(edited).await["kind"],
        morning_routine(&["kitchen-lights", "water-heater-plug"])
    );
}

#[tokio::test]
async fn a_routine_with_no_actions_or_an_assistant_prompt_is_a_bad_request() {
    let (app, _tmp) = make_app().await;
    let empty = serde_json::json!({"type": "routine", "actions": []});
    let with_prompt = serde_json::json!({
        "type": "routine",
        "actions": [{"type": "agent_prompt", "prompt": "say good morning"}],
    });
    app.clone()
        .oneshot(auth_post(
            "/api/v1/schedules",
            serde_json::json!({
                "id": "morning",
                "name": "Morning",
                "cron": "0 30 6 * * *",
                "kind": morning_routine(&["kitchen-lights"]),
            }),
        ))
        .await
        .unwrap();

    for kind in [empty, with_prompt] {
        let put = app
            .clone()
            .oneshot(auth_put(
                "/api/v1/schedules/morning",
                serde_json::json!({"kind": kind}),
            ))
            .await
            .unwrap();
        assert_eq!(put.status(), StatusCode::BAD_REQUEST, "{kind}");
    }
}
