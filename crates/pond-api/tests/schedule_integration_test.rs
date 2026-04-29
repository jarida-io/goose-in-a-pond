//! Integration tests for the scheduler REST routes:
//!   GET    /api/v1/schedules
//!   POST   /api/v1/schedules
//!   DELETE /api/v1/schedules/:id
//!   POST   /api/v1/schedules/:id/pause
//!   POST   /api/v1/schedules/:id/resume
//!   POST   /api/v1/schedules/:id/run-now
//!
//! Run: cargo test -p pond-api --test schedule_integration_test

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::domain::onboarding::OnboardingStep;
use pond_core::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::ports::onboarding::OnboardingRepository;
use pond_core::ports::scheduler::{CreateTaskRequest, ScheduledTask, SchedulerPort};
use pond_core::services::mock_agent::MockAgent;
use pond_core::services::mock_memory::MockMemoryRepository;
use pond_core::services::mock_profile::MockProfileRepository;
use pond_core::services::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::services::mock_settings::MockSettingsRepository;
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
    async fn get_current_step(&self) -> Option<OnboardingStep> { Some(OnboardingStep::Completed) }
    async fn save_step(&self, _: OnboardingStep) -> anyhow::Result<()> { Ok(()) }
    async fn reset(&self) -> anyhow::Result<()> { Ok(()) }
}

struct NoDevices;

#[async_trait::async_trait]
impl DeviceRegistry for NoDevices {
    async fn register(&self, req: RegisterDeviceRequest) -> anyhow::Result<Device> {
        Ok(Device {
            id: "mock".to_string(), name: req.name, device_type: req.device_type,
            hostname: req.hostname, ip_address: None, capabilities: req.capabilities,
            registered_at: "2024-01-01T00:00:00Z".to_string(), last_seen: None, is_online: false,
        })
    }
    async fn list_devices(&self) -> anyhow::Result<Vec<Device>> { Ok(vec![]) }
    async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> { Ok(None) }
    async fn unregister(&self, _: &str) -> anyhow::Result<()> { Ok(()) }
    async fn heartbeat(&self, _: &str) -> anyhow::Result<()> { Ok(()) }
}

// ── In-memory mock scheduler ──────────────────────────────────────────────────

struct InMemoryScheduler {
    tasks: Mutex<HashMap<String, ScheduledTask>>,
}

impl InMemoryScheduler {
    fn new() -> Self {
        Self { tasks: Mutex::new(HashMap::new()) }
    }
}

#[async_trait::async_trait]
impl SchedulerPort for InMemoryScheduler {
    async fn create_task(&self, req: CreateTaskRequest) -> anyhow::Result<ScheduledTask> {
        let mut guard = self.tasks.lock().await;
        if guard.contains_key(&req.id) {
            anyhow::bail!("task '{}' already exists", req.id);
        }
        let task = ScheduledTask {
            id: req.id.clone(),
            label: req.label,
            cron: req.cron,
            last_run: None,
            next_run: None,
            paused: false,
            currently_running: false,
            payload: Some(req.payload),
        };
        guard.insert(req.id, task.clone());
        Ok(task)
    }

    async fn list_tasks(&self) -> anyhow::Result<Vec<ScheduledTask>> {
        Ok(self.tasks.lock().await.values().cloned().collect())
    }

    async fn delete_task(&self, id: &str) -> anyhow::Result<()> {
        let mut guard = self.tasks.lock().await;
        guard.remove(id).map(|_| ()).ok_or_else(|| anyhow::anyhow!("not found"))
    }

    async fn pause_task(&self, id: &str) -> anyhow::Result<()> {
        let mut guard = self.tasks.lock().await;
        guard.get_mut(id)
            .map(|t| t.paused = true)
            .ok_or_else(|| anyhow::anyhow!("not found"))
    }

    async fn resume_task(&self, id: &str) -> anyhow::Result<()> {
        let mut guard = self.tasks.lock().await;
        guard.get_mut(id)
            .map(|t| t.paused = false)
            .ok_or_else(|| anyhow::anyhow!("not found"))
    }

    async fn run_now(&self, id: &str) -> anyhow::Result<()> {
        let guard = self.tasks.lock().await;
        guard.get(id).map(|_| ()).ok_or_else(|| anyhow::anyhow!("not found"))
    }
}

// ── App factory ───────────────────────────────────────────────────────────────

async fn make_app() -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let scheduler: Option<Arc<dyn SchedulerPort>> =
        Some(Arc::new(InMemoryScheduler::new()));

    let state = Arc::new(AppState {
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        session_storage,
        http_client: ReqwestClient::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
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
        download_tracker: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        piper_http_port: None,
        model_catalog_provider: None,
        model_storage_dir: None,
        prompt_template_repo: None,
        prompt_extra_repo: None,
        skill_repo: None,
        recipe_repo: None,
        llamafile_manager: None,
        event_log_repo: None,
        speaker_id: None,
        face_recognition: None,
        session_user_bindings: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        tool_agent: None,
        answer_reviewer: None,
    });
    (build_router(state, std::path::PathBuf::from("web/dist")), tmp)
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
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_schedules_returns_empty_array_initially() {
    let (app, _tmp) = make_app().await;

    let resp = app.oneshot(auth_get("/api/v1/schedules")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = json_body(resp).await;
    // Should be an array (possibly empty), NOT {"error":"..."}
    assert!(
        json.is_array(),
        "expected array from GET /schedules, got: {json}"
    );
}

#[tokio::test]
async fn create_schedule_returns_created_task() {
    let (app, _tmp) = make_app().await;

    let body = serde_json::json!({
        "id": "daily-summary",
        "label": "Daily Summary",
        "cron": "0 0 8 * * *",
        "payload": {"prompt": "Summarize yesterday's events"}
    });

    let resp = app
        .oneshot(auth_post("/api/v1/schedules", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = json_body(resp).await;
    assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("daily-summary"));
    assert_eq!(json.get("label").and_then(|v| v.as_str()), Some("Daily Summary"));
    assert_eq!(json.get("paused").and_then(|v| v.as_bool()), Some(false));
}

#[tokio::test]
async fn created_schedule_appears_in_list() {
    let (app, _tmp) = make_app().await;

    let body = serde_json::json!({
        "id": "weather-check",
        "label": "Weather Check",
        "cron": "0 0 7 * * *",
        "payload": {"prompt": "What's the weather today?"}
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
        tasks.iter().any(|t| t.get("id").and_then(|v| v.as_str()) == Some("weather-check")),
        "created task not found in list: {tasks:?}"
    );
}

#[tokio::test]
async fn delete_schedule_removes_it() {
    let (app, _tmp) = make_app().await;

    // Create
    let body = serde_json::json!({
        "id": "to-delete",
        "label": "To Delete",
        "cron": "0 0 9 * * *",
        "payload": {}
    });
    app.clone()
        .oneshot(auth_post("/api/v1/schedules", body))
        .await
        .unwrap();

    // Delete
    let del_resp = app
        .clone()
        .oneshot(auth_delete("/api/v1/schedules/to-delete"))
        .await
        .unwrap();
    assert_eq!(del_resp.status(), StatusCode::OK);

    // Should no longer be in list
    let list_resp = app.oneshot(auth_get("/api/v1/schedules")).await.unwrap();
    let json = json_body(list_resp).await;
    let tasks = json.as_array().unwrap();
    assert!(
        !tasks.iter().any(|t| t.get("id").and_then(|v| v.as_str()) == Some("to-delete")),
        "deleted task still in list: {tasks:?}"
    );
}

#[tokio::test]
async fn pause_and_resume_schedule() {
    let (app, _tmp) = make_app().await;

    // Create
    let body = serde_json::json!({
        "id": "pausable",
        "label": "Pausable",
        "cron": "0 0 10 * * *",
        "payload": {}
    });
    app.clone()
        .oneshot(auth_post("/api/v1/schedules", body))
        .await
        .unwrap();

    // Pause
    let pause_resp = app
        .clone()
        .oneshot(auth_post("/api/v1/schedules/pausable/pause", serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(pause_resp.status(), StatusCode::OK);

    // Verify paused in list
    let list_resp = app.clone().oneshot(auth_get("/api/v1/schedules")).await.unwrap();
    let json = json_body(list_resp).await;
    let task = json.as_array().unwrap()
        .iter()
        .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("pausable"))
        .expect("task not found");
    assert_eq!(task.get("paused").and_then(|v| v.as_bool()), Some(true));

    // Resume
    let resume_resp = app
        .clone()
        .oneshot(auth_post("/api/v1/schedules/pausable/resume", serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resume_resp.status(), StatusCode::OK);

    // Verify unpaused
    let list_resp2 = app.oneshot(auth_get("/api/v1/schedules")).await.unwrap();
    let json2 = json_body(list_resp2).await;
    let task2 = json2.as_array().unwrap()
        .iter()
        .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("pausable"))
        .expect("task not found after resume");
    assert_eq!(task2.get("paused").and_then(|v| v.as_bool()), Some(false));
}

#[tokio::test]
async fn run_now_returns_accepted() {
    let (app, _tmp) = make_app().await;

    // Create
    let body = serde_json::json!({
        "id": "run-now-task",
        "label": "Run Now",
        "cron": "0 0 11 * * *",
        "payload": {"prompt": "run this now"}
    });
    app.clone()
        .oneshot(auth_post("/api/v1/schedules", body))
        .await
        .unwrap();

    // Run now
    let run_resp = app
        .oneshot(auth_post("/api/v1/schedules/run-now-task/run-now", serde_json::json!({})))
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
