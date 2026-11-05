//! A sensor reading POSTed through the router is published on a live `InProcessEventBus`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use futures::StreamExt;
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::shared::services::in_process_event_bus::InProcessEventBus;
use pond_core::user_data::mocks::mock_device_registry::MockDeviceRegistry;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::onboarding::SqlxOnboardingRepository;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_skill::SqliteSkillRepository;
use tower::ServiceExt;

/// Router with a real event bus, returned so the test can subscribe.
async fn make_app_with_bus() -> (axum::Router, Arc<InProcessEventBus>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let db = Arc::new(db);

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let bus = Arc::new(InProcessEventBus::new());

    let state = Arc::new(AppState {
        warmup: Default::default(),
        room_voice: None,
        voice_match: None,
        suggestion_queue: std::sync::Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db,
        onboarding_repo: Arc::new(SqlxOnboardingRepository::new(pool.clone())),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage: Arc::new(SqliteSessionStorage::new(pool.clone())),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(MockDeviceRegistry),
        room_registry: Arc::new(pond_core::user_data::mocks::mock_room_registry::MockRoomRegistry),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedder: Default::default(),
        vector_index: None,
        index_reindex: None,
        lane: None,
        account_sync: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        face_recognition: None,
        prompt_template_dir: None,
        model_repo: None,
        data_dir: Some(tmp.path().to_path_buf()),
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
        prompt_template_repo: Some(Arc::new(SqlitePromptTemplateRepository::new(pool.clone()))),
        prompt_extra_repo: Some(Arc::new(SqlitePromptExtraRepository::new(pool.clone()))),
        skill_repo: Some(Arc::new(SqliteSkillRepository::new(pool.clone()))),
        recipe_repo: Some(Arc::new(SqliteRecipeRepository::new(pool.clone()))),
        llamafile_manager: None,
        operational_log: None,
        event_bus: Some(bus.clone() as Arc<dyn EventBus>),
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_history: None,
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
        setup_claims: None,
        spoken_codes: Default::default(),
        voice_samples: Default::default(),
        device_events: Default::default(),
        pond_admin: None,
    });

    // As `serve` does, so the stream route has the bus's device changes.
    state
        .device_events
        .clone()
        .follow(bus.clone() as Arc<dyn EventBus>);
    let router = build_router(state, std::path::PathBuf::from("pond-desktop/dist"));
    (router, bus, tmp)
}

#[tokio::test]
async fn sensor_post_publishes_to_event_bus() {
    let (app, bus, _tmp) = make_app_with_bus().await;
    let mut subscription = bus.subscribe();

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/sensors")
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "device_id": "backyard-pir",
                "sensor_type": "motion",
                "value": 1.0,
                "unit": "bool",
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "sensor POST should persist"
    );

    let event = tokio::time::timeout(Duration::from_secs(2), subscription.next())
        .await
        .expect("bus event within timeout")
        .expect("a bus event");

    match event {
        BusEvent::Sensor(reading) => {
            assert_eq!(reading.device_id, "backyard-pir");
            assert_eq!(reading.sensor_type, "motion");
        }
        other => panic!("expected BusEvent::Sensor, got {other:?}"),
    }
}

fn power(device: &str, on: bool) -> BusEvent {
    BusEvent::Device(pond_core::user_data::domain::device::DeviceStateChanged {
        device_id: pond_core::user_data::domain::device::DeviceId::new(device),
        key: "power".into(),
        value: pond_core::user_data::domain::device::DeviceStateValue::Bool(on),
        changed_at: chrono::Utc::now(),
    })
}

/// The stream's response, opened as the app opens it.
async fn open_device_events(
    app: &axum::Router,
    after_seq: Option<u64>,
) -> axum::response::Response {
    let uri = match after_seq {
        Some(seq) => format!("/api/v1/devices/events?after_seq={seq}"),
        None => "/api/v1/devices/events".to_string(),
    };
    let request = Request::builder()
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response
}

/// The first `count` events on a stream, as `(event, data)`.
async fn sse_events(
    response: axum::response::Response,
    count: usize,
) -> Vec<(String, serde_json::Value)> {
    let mut body = response.into_body().into_data_stream();
    let mut text = String::new();
    let mut events = Vec::new();
    while events.len() < count {
        let chunk = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("an event within the timeout")
            .expect("the stream stays open")
            .unwrap();
        text.push_str(std::str::from_utf8(&chunk).unwrap());
        while let Some(end) = text.find("\n\n") {
            let block: String = text.drain(..end + 2).collect();
            let field = |name: &str| {
                block
                    .lines()
                    .find_map(|line| line.strip_prefix(name))
                    .map(|v| v.trim().to_string())
            };
            if let (Some(event), Some(data)) = (field("event:"), field("data:")) {
                events.push((event, serde_json::from_str(&data).unwrap()));
            }
        }
    }
    events
}

/// Lets the feed's follower and an opened stream's subscription catch up.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test]
async fn a_device_state_change_reaches_an_open_device_stream() {
    let (app, bus, _tmp) = make_app_with_bus().await;
    let stream = open_device_events(&app, None).await;
    settle().await;
    bus.publish(power("matter-1", true));

    let events = sse_events(stream, 1).await;
    assert_eq!(events[0].0, "device_state");
    let data = &events[0].1;
    assert_eq!(data["device_id"], "matter-1");
    assert_eq!(data["key"], "power");
    assert_eq!(data["value"], true);
    assert!(data["seq"].as_u64().is_some(), "{data}");
}

#[tokio::test]
async fn a_phone_resumes_after_the_last_change_it_saw() {
    let (app, bus, _tmp) = make_app_with_bus().await;
    let stream = open_device_events(&app, None).await;
    settle().await;
    for on in [true, false, true] {
        bus.publish(power("matter-1", on));
    }
    let seen = sse_events(stream, 3).await;
    let first = seen[0].1["seq"].as_u64().unwrap();

    let resumed = sse_events(open_device_events(&app, Some(first)).await, 2).await;
    let seqs: Vec<_> = resumed
        .iter()
        .map(|(_, d)| d["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, vec![first + 1, first + 2]);
    assert_eq!(resumed[0].1["value"], false);
}

#[tokio::test]
async fn a_number_from_before_a_restart_asks_for_a_resync() {
    let (app, _bus, _tmp) = make_app_with_bus().await;
    let events = sse_events(open_device_events(&app, Some(7)).await, 1).await;
    assert_eq!(events[0].0, "resync");
}
