//! HTTP-level coverage for the household role & guest-device-access routes
//! (`household_permission.rs` only covers the pure policy functions; this exercises the
//! actual handlers, AppState wiring, the last-owner guard, and the 404/403 paths).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_settings::SqliteSettingsRepository;
use pond_infra::sqlite_skill::SqliteSkillRepository;
use tower::ServiceExt;

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

/// One fixed device, "lamp-1", so the guest-device-access routes have something real to grant.
struct OneLamp;

#[async_trait::async_trait]
impl DeviceRegistry for OneLamp {
    async fn register(&self, req: RegisterDeviceRequest) -> anyhow::Result<Device> {
        Ok(Device {
            id: "lamp-1".into(),
            name: req.name,
            device_type: req.device_type,
            hostname: req.hostname,
            ip_address: None,
            capabilities: req.capabilities,
            registered_at: "2024-01-01 00:00:00".into(),
            last_seen: None,
            is_online: false,
            room_id: req.room_id,
        })
    }
    async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        Ok(vec![self.get_device("lamp-1").await?.unwrap()])
    }
    async fn get_device(&self, id: &str) -> anyhow::Result<Option<Device>> {
        if id == "lamp-1" {
            Ok(Some(Device {
                id: "lamp-1".into(),
                name: "Living Room Lamp".into(),
                device_type: "light".into(),
                hostname: Some("lamp-1.local".into()),
                ip_address: None,
                capabilities: vec![],
                registered_at: "2024-01-01 00:00:00".into(),
                last_seen: None,
                is_online: true,
                room_id: None,
            }))
        } else {
            Ok(None)
        }
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
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let db = Arc::new(db);

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let session_storage = Arc::new(SqliteSessionStorage::new(pool.clone()));

    let state = Arc::new(AppState {
        warmup: Default::default(),
        suggestion_queue: Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db,
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage: session_storage.clone(),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(SqliteSettingsRepository::new(pool.clone())),
        profile_repo: Arc::new(SqliteProfileRepository::new(pool.clone())),
        device_registry: Arc::new(OneLamp),
        room_registry: Arc::new(pond_core::user_data::mocks::mock_room_registry::MockRoomRegistry),
        guest_room_access: Arc::new(pond_core::user_data::mocks::mock_guest_room_access::MockGuestRoomAccess),
        guest_device_access: Arc::new(pond_infra::sqlite_guest_device_access::SqliteGuestDeviceAccess::new(pool.clone())),
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
        event_bus: None,
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
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
    });

    (
        build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        tmp,
    )
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap()
}

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn put(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(Method::PUT)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

async fn show(label: &str, router: &axum::Router, req: Request<Body>) -> serde_json::Value {
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    println!("--- {label} ---\nHTTP {status}\n{}\n", serde_json::to_string_pretty(&body).unwrap());
    body
}

/// Walks the whole feature over real HTTP: creating profiles, setting roles, the last-owner
/// guard, granting/revoking a guest's device access, and the security boundary that appears
/// once a household has more than one member and the caller carries no proven device.
#[tokio::test]
async fn household_roles_and_guest_device_access_over_http() {
    let (router, _tmp) = make_app().await;

    // 1. Fresh pond: no profiles yet.
    let body = show("1. GET /profiles (empty)", &router, get("/api/v1/profiles")).await;
    assert_eq!(body["profiles"].as_array().unwrap().len(), 0);

    // 2. Create the first profile. The first profile in an empty household is its owner.
    let body = show(
        "2. POST /profiles -> Ada",
        &router,
        post(
            "/api/v1/profiles",
            serde_json::json!({"display_name": "Ada", "avatar_emoji": "\u{1F469}\u{200D}\u{1F4BB}"}),
        ),
    )
    .await;
    let owner_id = body["id"].as_str().unwrap().to_string();
    assert_eq!(
        body["role"], "owner",
        "the first profile in an empty household is its owner"
    );

    // 3a. Guest-device-access routes, exercised while Ada is still legitimately Owner.
    let body = show(
        "3a. GET /profiles/{Ada}/devices (empty)",
        &router,
        get(&format!("/api/v1/profiles/{owner_id}/devices")),
    )
    .await;
    assert_eq!(body["device_ids"].as_array().unwrap().len(), 0);

    let body = show(
        "3b. PUT /profiles/{Ada}/devices/lamp-1 -> grant",
        &router,
        put(&format!("/api/v1/profiles/{owner_id}/devices/lamp-1"), serde_json::json!({})),
    )
    .await;
    assert_eq!(body["device_ids"].as_array().unwrap(), &vec![serde_json::json!("lamp-1")]);

    let resp = router
        .clone()
        .oneshot(put(
            &format!("/api/v1/profiles/{owner_id}/devices/no-such-device"),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    println!(
        "--- 3c. PUT /profiles/{{Ada}}/devices/no-such-device (expect 404) ---\nHTTP {}\n",
        resp.status()
    );
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let req = Request::builder()
        .method(Method::DELETE)
        .uri(format!("/api/v1/profiles/{owner_id}/devices/lamp-1"))
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap();
    let body = show("3d. DELETE /profiles/{Ada}/devices/lamp-1 -> revoke", &router, req).await;
    assert_eq!(body["device_ids"].as_array().unwrap().len(), 0);

    // 4. Try to demote Ada -- she's the only owner. Refused by the last-owner guard.
    let resp = router
        .clone()
        .oneshot(put(
            &format!("/api/v1/profiles/{owner_id}/role"),
            serde_json::json!({"role": "member"}),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    println!(
        "--- 4. PUT /profiles/{{Ada}}/role {{role: member}} (expect 409, last owner) ---\nHTTP {status}\n{}\n",
        serde_json::to_string_pretty(&body).unwrap()
    );
    assert_eq!(status, StatusCode::CONFLICT);

    // 4b. Try to delete Ada outright -- same guard applies to removal, not just demotion.
    let req = Request::builder()
        .method(Method::DELETE)
        .uri(format!("/api/v1/profiles/{owner_id}"))
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    println!(
        "--- 4b. DELETE /profiles/{{Ada}} (expect 409, last owner) ---\nHTTP {status}\n{}\n",
        serde_json::to_string_pretty(&body).unwrap()
    );
    assert_eq!(status, StatusCode::CONFLICT);

    // 5. Create a second profile: now the household has 2 members.
    let body = show(
        "5. POST /profiles -> Visiting Friend",
        &router,
        post(
            "/api/v1/profiles",
            serde_json::json!({"display_name": "Visiting Friend", "avatar_emoji": "\u{1F9F3}"}),
        ),
    )
    .await;
    let guest_id = body["id"].as_str().unwrap().to_string();

    // At this point the test's "Bearer test-token" caller carries no proven device, so the
    // moment a second profile exists, this caller resolves to Guest scope, not Owner -- the
    // same boundary demonstrated earlier for an anonymous caller.
    let resp = router
        .clone()
        .oneshot(put(
            &format!("/api/v1/profiles/{guest_id}/role"),
            serde_json::json!({"role": "guest"}),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    println!(
        "--- 6. PUT /profiles/{{Guest}}/role as an unidentified caller in a 2-member household (expect 403) ---\nHTTP {status}\n{}\n",
        serde_json::to_string_pretty(&body).unwrap()
    );
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["owners"].as_array().unwrap(),
        &vec![serde_json::json!("Ada")],
        "a refusal names the actual owner(s), not just the role"
    );

    println!(
        "Household now has {{Ada: owner}} and {{Visiting Friend: member (default)}}. \
         The 403 above is the feature working as designed: once a household has more than one \
         member, an unidentified caller is never trusted as its owner."
    );
}
