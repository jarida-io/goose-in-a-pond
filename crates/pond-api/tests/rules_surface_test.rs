//! `/api/v1/rules` shares the schedules store; only `find_rule`'s `rule_view` filter keeps
//! `DELETE /rules/{id}` off a schedule such as the nightly backup.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::schedule::{Schedule, ScheduleRun};
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
        let schedule = Schedule {
            fire_at: None,
            id: req.id.clone(),
            label: req.label,
            cron: req.cron,
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

    async fn resume_task(&self, id: &str) -> anyhow::Result<()> {
        let mut guard = self.tasks.lock().await;
        guard
            .get_mut(id)
            .map(|t| t.paused = false)
            .ok_or_else(|| anyhow::anyhow!("not found"))
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
        Ok(task.clone())
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
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let scheduler: Option<Arc<dyn SchedulerPort>> = Some(Arc::new(InMemoryScheduler::new()));

    let state = Arc::new(AppState {
        warmup: Default::default(),
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
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
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

// ── Fixtures ──────────────────────────────────────────────────────────────────

/// A rule the domain accepts: one source, one action, no time window.
fn valid_rule(name: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "source": {"kind": "sensor", "device_id": "backyard-pir", "signal": "motion"},
        "actions": [{"type": "notify", "title": "Motion", "body": "Backyard"}],
        "cooldown_secs": 60
    })
}

/// The nightly backup, a cron schedule whose id any caller can learn from `GET /schedules`.
async fn create_backup(app: &axum::Router) -> String {
    let resp = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/schedules",
            serde_json::json!({
                "name": "Nightly backup",
                "cron": "0 0 3 * * *",
                "prompt": "back up the pond"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    json_body(resp).await["id"].as_str().unwrap().to_string()
}

async fn create_rule(app: &axum::Router, name: &str) -> String {
    let resp = app
        .clone()
        .oneshot(auth_post("/api/v1/rules", valid_rule(name)))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "the positive control failed: {}",
        json_body(resp).await
    );
    json_body(resp).await["id"].as_str().unwrap().to_string()
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

async fn schedule_by_id(app: &axum::Router, id: &str) -> Option<serde_json::Value> {
    let resp = app
        .clone()
        .oneshot(auth_get("/api/v1/schedules"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    json_body(resp)
        .await
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .cloned()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_rules_surface_is_not_a_second_door_onto_the_schedules_store() {
    let (app, _tmp) = make_app().await;
    let backup = create_backup(&app).await;
    // Created first, so the 404s below cannot mean an empty store.
    let rule = create_rule(&app, "Backyard motion").await;

    let doors: Vec<(&str, Request<Body>)> = vec![
        (
            "GET /rules/{id}",
            auth_get(&format!("/api/v1/rules/{backup}")),
        ),
        (
            "PUT /rules/{id}",
            auth_put(&format!("/api/v1/rules/{backup}"), valid_rule("hijacked")),
        ),
        (
            "DELETE /rules/{id}",
            auth_delete(&format!("/api/v1/rules/{backup}")),
        ),
        (
            "POST /rules/{id}/pause",
            auth_post(
                &format!("/api/v1/rules/{backup}/pause"),
                serde_json::json!({}),
            ),
        ),
        (
            "POST /rules/{id}/resume",
            auth_post(
                &format!("/api/v1/rules/{backup}/resume"),
                serde_json::json!({}),
            ),
        ),
    ];
    for (name, req) in doors {
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "{name} reached a cron schedule: the household's nightly backup can \
             be read, rewritten, paused or deleted through /rules by guessing \
             its id, which GET /schedules hands out"
        );
    }

    // A 404 that acted anyway is the real failure, so ask the store directly.
    let after = schedule_by_id(&app, &backup)
        .await
        .expect("the nightly backup was DELETED through /rules despite the 404");
    assert_eq!(after["paused"], serde_json::json!(false), "{after}");
    assert_eq!(after["cron"], serde_json::json!("0 0 3 * * *"), "{after}");
    assert!(
        after["kind"]["prompt"].is_string(),
        "the backup was rewritten into a sensor rule through PUT /rules: it now \
         fires the caller's actions on the backup's schedule. {after}"
    );

    // Vacuity control: the same five doors answer for a real rule.
    let resp = app
        .clone()
        .oneshot(auth_get(&format!("/api/v1/rules/{rule}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(auth_post(
            &format!("/api/v1/rules/{rule}/pause"),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(auth_delete(&format!("/api/v1/rules/{rule}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn every_door_that_stores_a_rule_refuses_one_that_can_never_fire() {
    let (app, _tmp) = make_app().await;
    // The scheduler here stores anything, so a 400 can only come from the handler.
    let rule = create_rule(&app, "Backyard motion").await;

    let actionless = {
        let mut spec = valid_rule("Actionless");
        spec["actions"] = serde_json::json!([]);
        spec
    };
    let malformed_window = {
        let mut spec = valid_rule("After sunset");
        spec["condition"] = serde_json::json!({"after": "half six"});
        spec
    };

    let doors: Vec<(&str, Request<Body>, &str)> = vec![
        (
            "POST /rules (no actions)",
            auth_post("/api/v1/rules", actionless.clone()),
            "action",
        ),
        (
            "PUT /rules/{id} (no actions)",
            auth_put(&format!("/api/v1/rules/{rule}"), actionless.clone()),
            "action",
        ),
        (
            "PUT /rules/{id} (unparseable time bound)",
            auth_put(&format!("/api/v1/rules/{rule}"), malformed_window.clone()),
            "after",
        ),
        (
            // The older door: a rule can arrive here by naming the kind.
            "POST /schedules (no actions)",
            auth_post(
                "/api/v1/schedules",
                serde_json::json!({
                    "name": "Actionless",
                    "cron": "@event",
                    "kind": {"type": "sensor_trigger",
                             "source": {"kind": "sensor"},
                             "actions": []}
                }),
            ),
            "action",
        ),
    ];
    for (name, req, must_name) in doors {
        let resp = app.clone().oneshot(req).await.unwrap();
        // Status first: every lookup on an error body answers `None`.
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "{name} did not refuse a rule that can never fire"
        );
        let body = json_body(resp).await;
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.contains(must_name),
            "{name} refused without naming the field: the caller is sent to the \
             logs to find out what is wrong. Got {body}"
        );
    }

    // Nothing was stored on the way past: only the control rule, unchanged.
    let resp = app
        .clone()
        .oneshot(auth_get("/api/v1/rules"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let rules = json_body(resp).await;
    let rules = rules.as_array().unwrap();
    assert_eq!(
        rules.len(),
        1,
        "a refused rule was stored anyway: {rules:?}"
    );
    assert_eq!(rules[0]["id"], serde_json::json!(rule));
    assert_eq!(
        rules[0]["actions"].as_array().map(Vec::len),
        Some(1),
        "the refused spec overwrote the stored one: {}",
        rules[0]
    );
    assert!(
        rules[0]["condition"]["after"].is_null(),
        "the refused time window was stored: {}",
        rules[0]
    );

    // Vacuity control: the same PUT with a valid spec is accepted.
    let resp = app
        .clone()
        .oneshot(auth_put(
            &format!("/api/v1/rules/{rule}"),
            valid_rule("Backyard motion, renamed"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "the control PUT was refused");
}
