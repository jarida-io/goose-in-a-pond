//! Integration tests for GET/PUT /api/v1/settings.
//!
//! PUT must return the full Settings object rather than a status stub, a partial patch must
//! preserve unmodified fields, and a following GET must show what the PUT wrote.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use reqwest::Client as ReqwestClient;
use std::sync::Arc;
use tower::ServiceExt;

// ── Stubs ──────────────────────────────────────────────────────────────���───────

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
    // PAI-2 P7 made this a required trait method rather than a defaulted one:
    // a default would have to answer from `get_current_step`, and a stub that
    // answers "not onboarded" makes every onboarding write route public
    // wherever it is used. The name of this stub is the answer.
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
            registered_at: "2024-01-01 00:00:00".to_string(),
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

async fn make_app() -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
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
        lane: None,
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
    });
    (
        build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        tmp,
    )
}

// ── Tests ──────────────────────────────────────────────────────────────────────

/// PUT /api/v1/settings must return the full Settings object, not {"status":"ok"}.
/// The frontend calls updateSettings() and expects a Settings type back.
#[tokio::test]
async fn put_settings_returns_full_settings_object() {
    let (app, _tmp) = make_app().await;

    let patch = serde_json::json!({
        "assistant_name": "Jarvis"
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/settings")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(serde_json::to_vec(&patch).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    // Must be a Settings object — NOT {"status":"ok"}
    assert!(
        json.get("status").and_then(|s| s.as_str()) != Some("ok"),
        "PUT /settings returned {{\"status\":\"ok\"}} — should return full Settings"
    );

    // Should have known Settings fields
    assert!(
        json.get("assistant_name").is_some() || json.get("user_name").is_some(),
        "Response doesn't look like a Settings object: {json}"
    );

    // The patched field should be reflected
    assert_eq!(
        json.get("assistant_name").and_then(|v| v.as_str()),
        Some("Jarvis"),
        "assistant_name not updated in response: {json}"
    );
}

/// Partial patch preserves other fields.
#[tokio::test]
async fn put_settings_partial_patch_preserves_other_fields() {
    let (app, _tmp) = make_app().await;

    // First: set both fields
    let patch1 = serde_json::json!({
        "assistant_name": "Pond",
        "user_name": "Jerry"
    });
    let resp1 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/settings")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(serde_json::to_vec(&patch1).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);

    // Second: patch only assistant_name
    let patch2 = serde_json::json!({ "assistant_name": "Goose" });
    let resp2 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/settings")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(serde_json::to_vec(&patch2).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp2.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(
        json.get("assistant_name").and_then(|v| v.as_str()),
        Some("Goose"),
    );
    // user_name from patch1 should be preserved
    assert_eq!(
        json.get("user_name").and_then(|v| v.as_str()),
        Some("Jerry"),
        "user_name was lost after partial patch"
    );
}

/// GET /api/v1/settings returns current settings (including previously PUT values).
#[tokio::test]
async fn get_settings_returns_current_settings() {
    let (app, _tmp) = make_app().await;

    // Put a value
    let patch = serde_json::json!({ "assistant_name": "Ducky" });
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/settings")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(serde_json::to_vec(&patch).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    // Get settings
    let get_resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/settings")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(get_resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(get_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(
        json.get("assistant_name").and_then(|v| v.as_str()),
        Some("Ducky"),
    );
}

/// GET /api/v1/weather must report {"enabled": false} rather than error
/// when no weather provider is configured (the default in tests / for
/// users who haven't set a location).
#[tokio::test]
async fn get_weather_reports_disabled_without_provider() {
    let (app, _tmp) = make_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/weather")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json.get("enabled").and_then(|v| v.as_bool()), Some(false));
}

/// PAI-2 P2: no credential material may come back out of `GET /settings`. The handler is
/// `serde_json::to_value(settings)` with no DTO and no redaction, so this is a property of
/// the struct. The pond-core guard `no_settings_field_is_secret_shaped` asserts it against
/// the default; this asserts it over real HTTP after a write, on a configured value.
#[tokio::test]
async fn settings_response_never_carries_a_secret_shaped_key() {
    let (app, _tmp) = make_app().await;

    // An old client (or a stale phone build) still sends the legacy field.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/settings")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "api_key_guardian": "leaked-guardian-key",
                        "assistant_name": "Jarvis"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "an unknown field must not break the save for a client that has not been updated"
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/settings")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let obj = body.as_object().expect("settings is a JSON object");

    // POSITIVE CONTROL, first: the request really reached the settings store.
    // Without this, an error payload or an empty object satisfies both of the
    // negative assertions below and the test reports the opposite of the truth.
    assert_eq!(
        obj.get("assistant_name").and_then(|v| v.as_str()),
        Some("Jarvis"),
        "the GET did not return real settings, so nothing below means anything"
    );

    const SECRET_WORDS: &[&str] = &[
        "key",
        "keys",
        "token",
        "tokens",
        "secret",
        "secrets",
        "password",
        "passwords",
        "credential",
        "credentials",
        "apikey",
        "passphrase",
    ];
    // A token BUDGET / an exchange RATE / a token COUNT ceiling, not a
    // bearer token. Mirrors NOT_ACTUALLY_SECRET in pond-core's guard; if the
    // two ever disagree, one of them is wrong.
    const NOT_ACTUALLY_SECRET: &[&str] = &[
        "llm_max_tokens",
        "mesh_settlement_millisats_per_token",
        "mesh_lend_token_ceiling",
    ];

    let leaked: Vec<&String> = obj
        .keys()
        .filter(|k| {
            k.split('_').any(|seg| SECRET_WORDS.contains(&seg))
                && !NOT_ACTUALLY_SECRET.contains(&k.as_str())
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "GET /api/v1/settings returned secret-shaped field(s): {leaked:?}"
    );
    assert!(
        !String::from_utf8_lossy(&bytes).contains("leaked-guardian-key"),
        "the value an old client sent came straight back out of GET /settings"
    );
}

/// PAI-2 P5. `NetworkMode::parse` widens on an unrecognised value, on purpose -- a typo must
/// not silently take a home assistant off the internet. So this 422 is the only thing between
/// a typo and a gate that is quietly off. The status code is asserted BEFORE any body
/// predicate: a body-shape check alone passes against an error payload.
#[tokio::test]
async fn put_settings_refuses_an_unrecognised_network_mode() {
    let (app, _tmp) = make_app().await;

    async fn put(app: &axum::Router, body: serde_json::Value) -> axum::http::Response<Body> {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/settings")
                    .header("content-type", "application/json")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn stored_mode(app: &axum::Router) -> String {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/settings")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        json.get("network_mode")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("GET /settings carried no network_mode: {json}"))
            .to_string()
    }

    // Positive control first: the field exists and defaults to the open,
    // status-quo-preserving value. Without this the assertions below could pass
    // against a Settings struct that never grew the field.
    assert_eq!(stored_mode(&app).await, "open");

    let bad = put(&app, serde_json::json!({ "network_mode": "offlien" })).await;
    assert_eq!(
        bad.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unrecognised network_mode must be refused, not absorbed into \"open\""
    );
    assert_eq!(
        stored_mode(&app).await,
        "open",
        "the refused value must not have reached the store"
    );

    // ...and a recognised value still round-trips, or the guard has locked the
    // setting out entirely, which is a different bug wearing the same green.
    let good = put(&app, serde_json::json!({ "network_mode": "offline" })).await;
    assert_eq!(good.status(), StatusCode::OK);
    assert_eq!(stored_mode(&app).await, "offline");

    // The handler installs the mode process-wide. Put it back, so a later test
    // in this binary does not inherit an offline pond.
    let restore = put(&app, serde_json::json!({ "network_mode": "open" })).await;
    assert_eq!(restore.status(), StatusCode::OK);
}

/// PAI-5 P4. `ReasoningEffort::parse` narrows on an unrecognised value, so the fallback is
/// safe but silent: without this 422 a typo leaves the user on the smallest think with no
/// way to tell why. Persistence is out of scope here — `MockSettingsRepository` carries a
/// hand-maintained subset — and is guarded by `roundtrip_persists_every_field` in pond-infra.
#[tokio::test]
async fn put_settings_refuses_an_unrecognised_reasoning_effort() {
    let (app, _tmp) = make_app().await;

    async fn put(app: &axum::Router, body: serde_json::Value) -> axum::http::Response<Body> {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/settings")
                    .header("content-type", "application/json")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn stored_effort(app: &axum::Router) -> String {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/settings")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        json.get("reasoning_effort")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("GET /settings carried no reasoning_effort: {json}"))
            .to_string()
    }

    // Positive control: the field exists and defaults to the on-device value.
    // Without it the assertions below would pass against a Settings struct that
    // never grew the field, because every lookup on a missing key is None.
    assert_eq!(stored_effort(&app).await, "brief");

    let bad = put(&app, serde_json::json!({ "reasoning_effort": "thourough" })).await;
    assert_eq!(
        bad.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unrecognised reasoning_effort must be refused, not absorbed into \"brief\""
    );
    assert_eq!(
        stored_effort(&app).await,
        "brief",
        "the refused value must not have reached the store"
    );

    // ...and every recognised value is accepted and comes back in the merged
    // object the handler returns, or the guard has locked the setting out
    // entirely — a different bug wearing the same green.
    for good in ["balanced", "thorough", "brief"] {
        let resp = put(&app, serde_json::json!({ "reasoning_effort": good })).await;
        assert_eq!(resp.status(), StatusCode::OK, "PUT {good} was refused");
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("reasoning_effort").and_then(|v| v.as_str()),
            Some(good),
            "PUT returned 200 but the merged settings do not carry {good:?}: {json}"
        );
    }
}
