//! #114 acceptance: `GET /api/v1/activity` answers "what happened" with
//! filtering, and `/activity/summary` answers "last hour/day/week". Drives a
//! real router with a live `SqliteEventLog` wired into `AppState`. Also asserts
//! the security rule that `Secret`-classified events are never surfaced.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::security::domain::event::{Event, EventCategory, PrivacySensitivity};
use pond_core::security::ports::event_log::EventLog;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::mocks::mock_device_registry::MockDeviceRegistry;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::onboarding::SqlxOnboardingRepository;
use pond_infra::sqlite_event_log::SqliteEventLog;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_skill::SqliteSkillRepository;
use tower::ServiceExt;

/// Build the router with a real `SqliteEventLog` pre-seeded with a few events
/// (two visible, one `Secret`). Returns the router + tempdir guard.
async fn make_app() -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();

    // Seed the unified event store.
    let event_log: Arc<dyn EventLog> = Arc::new(SqliteEventLog::new(db.logs.clone()));
    event_log
        .append(
            Event::new(EventCategory::Sensor, "sensor.reading")
                .attr("device_id", "backyard-pir")
                .session("sess-a"),
        )
        .await
        .unwrap();
    event_log
        .append(Event::new(EventCategory::Device, "device.state_changed").attr("device_id", "lamp"))
        .await
        .unwrap();
    // Secret-classified — must never be surfaced by the API.
    event_log
        .append(
            Event::new(EventCategory::Auth, "auth.token_minted")
                .sensitivity(PrivacySensitivity::Secret),
        )
        .await
        .unwrap();

    let db = Arc::new(db);
    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        db,
        onboarding_repo: Arc::new(SqlxOnboardingRepository::new(pool.clone())),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        session_storage: Arc::new(SqliteSessionStorage::new(pool.clone())),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(MockDeviceRegistry),
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
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
        event_log_repo: None,
        event_bus: None,
        event_log: Some(event_log.clone()),
        session_user_bindings: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
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
        security_policy: None,
        tool_dispatcher: None,
        api_port: 4000,
    });

    (
        build_router(state, std::path::PathBuf::from("web/dist")),
        tmp,
    )
}

async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn activity_lists_events_and_hides_secret() {
    let (app, _tmp) = make_app().await;

    let (status, body) = get_json(&app, "/api/v1/activity").await;
    assert_eq!(status, StatusCode::OK);
    // Two seeded events are visible; the Secret one is excluded.
    assert_eq!(body["count"], 2, "Secret event must be hidden");
    let actions: Vec<&str> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert!(actions.contains(&"sensor.reading"));
    assert!(actions.contains(&"device.state_changed"));
    assert!(
        !actions.contains(&"auth.token_minted"),
        "Secret action leaked"
    );
}

#[tokio::test]
async fn activity_filters_by_category_and_session() {
    let (app, _tmp) = make_app().await;

    let (status, body) = get_json(&app, "/api/v1/activity?category=sensor").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 1);
    assert_eq!(body["events"][0]["action"], "sensor.reading");

    let (_, by_session) = get_json(&app, "/api/v1/activity?session_id=sess-a").await;
    assert_eq!(by_session["count"], 1);
}

#[tokio::test]
async fn activity_summary_counts_by_category() {
    let (app, _tmp) = make_app().await;

    let (status, body) = get_json(&app, "/api/v1/activity/summary?window=day").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 2, "Secret event excluded from totals");
    assert_eq!(body["by_category"]["sensor"], 1);
    assert_eq!(body["by_category"]["device"], 1);
    assert!(body["by_category"].get("auth").is_none());
}

#[tokio::test]
async fn activity_rejects_bad_params() {
    let (app, _tmp) = make_app().await;
    let (status, _) = get_json(&app, "/api/v1/activity?category=bogus").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = get_json(&app, "/api/v1/activity?since=not-a-date").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = get_json(&app, "/api/v1/activity/summary?window=decade").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
