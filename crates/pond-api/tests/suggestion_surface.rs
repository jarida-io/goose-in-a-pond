//! The suggestion route over HTTP: the claims the pure `suggestion` service tests can't make.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_profile::SqliteProfileRepository;
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

/// Gives `devices_online` something real to count; `NoDevices` would make tests vacuous.
struct SomeDevices(usize);

#[async_trait::async_trait]
impl DeviceRegistry for SomeDevices {
    async fn register(&self, _: RegisterDeviceRequest) -> anyhow::Result<Device> {
        anyhow::bail!("not needed by this suite")
    }
    async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        Ok((0..self.0)
            .map(|i| Device {
                id: format!("d{i}"),
                name: format!("Device {i}"),
                device_type: "light".to_string(),
                hostname: None,
                ip_address: None,
                capabilities: vec![],
                registered_at: chrono::Utc::now().to_rfc3339(),
                last_seen: None,
                is_online: true,
                room_id: None,
            })
            .collect())
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
    profiles: Arc<SqliteProfileRepository>,
    pool: sqlx::Pool<sqlx::Sqlite>,
    storage: Arc<SqliteSessionStorage>,
    settings: Arc<pond_infra::sqlite_settings::SqliteSettingsRepository>,
    _tmp: tempfile::TempDir,
}

async fn make_app(device_count: usize) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let storage = Arc::new(SqliteSessionStorage::new(pool.clone()));
    let profiles = Arc::new(SqliteProfileRepository::new(pool.clone()));
    // Not `MockSettingsRepository`: it persists few fields, not `suggestion_generation_enabled`.
    let settings = Arc::new(pond_infra::sqlite_settings::SqliteSettingsRepository::new(
        pool.clone(),
    ));
    let devices: Arc<dyn DeviceRegistry + Send + Sync> = Arc::new(SomeDevices(device_count));
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
        settings_repo: settings.clone(),
        profile_repo: profiles.clone(),
        device_registry: devices,
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
        pond_admin: None,
    });

    Harness {
        app: build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        profiles,
        pool,
        storage,
        settings,
        _tmp: tmp,
    }
}

async fn member(h: &Harness, name: &str) -> String {
    h.profiles
        .create(CreateProfileRequest {
            display_name: name.to_string(),
            avatar_emoji: "\u{1F986}".to_string(),
        })
        .await
        .unwrap()
        .id
}

async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

/// A session bound to `profile_id` at `Explicit` strength, as `PUT /sessions/{id}/user` does.
async fn session_of(h: &Harness, id: &str, profile_id: &str) -> String {
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

/// Queue a question composed from `member`'s own note, as the lane would.
/// The note goes in the real `memory_fragments`: `offerable` joins it to check it's live.
async fn compose_for(h: &Harness, member: &str, memory_id: &str, prompt: &str) {
    use pond_core::user_data::ports::suggestion_queue::SuggestionQueueRepository;
    use pond_core::user_data::services::suggestion_generation::GeneratedSuggestion;
    sqlx::query(
        "INSERT INTO memory_fragments (id, profile_id, content, source, created_at, lifecycle) \
         VALUES (?, ?, 'a private note', 'extraction', datetime('now'), 'active')",
    )
    .bind(memory_id)
    .bind(member)
    .execute(&h.pool)
    .await
    .unwrap();
    let queued = pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(h.pool.clone())
        .queue(&[GeneratedSuggestion {
            source_memory_id: memory_id.to_string(),
            profile_id: Some(member.to_string()),
            prompt: prompt.to_string(),
            reason: "From something you told me, saved 3 days ago.".to_string(),
        }])
        .await
        .unwrap();
    assert_eq!(queued, 1, "the fixture must actually queue the question");
}

fn prompts(body: &Value) -> Vec<String> {
    body["suggestions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s["prompt"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn offered(body: &Value) -> Vec<String> {
    body["suggestions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn considered(body: &Value) -> Vec<String> {
    body["considered"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// ── The claims ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_route_answers_with_no_session_at_all() {
    let h = make_app(19).await;
    let _jerry = member(&h, "Jerry").await;

    let (status, body) = get_json(&h.app, "/api/v1/suggestions").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a cold Dashboard has no session and this is the surface that has to fill its \
         column anyway. Body: {body}"
    );
    assert!(
        body["suggestions"].is_array(),
        "the shape must be stable even when nothing is offered; body: {body}"
    );
}

#[tokio::test]
async fn considered_names_every_suggestor_even_when_all_are_silent() {
    // No extensions on this harness, so every suggestor is silent: the worst case to pin.
    let h = make_app(0).await;
    let _jerry = member(&h, "Jerry").await;

    let (status, body) = get_json(&h.app, "/api/v1/suggestions").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        offered(&body).is_empty(),
        "nothing should be offered with no extensions and no data; body: {body}"
    );
    assert!(
        considered(&body).len() >= 7,
        "an empty column must still say what was looked at, or a quiet house and a \
         broken engine are the same response. Body: {body}"
    );
    for entry in body["considered"].as_array().unwrap() {
        let reason = entry["silent_because"].as_str().unwrap_or_default();
        assert!(
            !reason.trim().is_empty(),
            "{} went silent without saying why; body: {body}",
            entry["id"]
        );
    }
}

#[tokio::test]
async fn a_multi_member_pond_is_offered_nothing_personal() {
    let h = make_app(19).await;
    let _liz = member(&h, "Liz").await;
    let _jerry = member(&h, "Jerry").await;

    let (status, body) = get_json(&h.app, "/api/v1/suggestions").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["audience"].as_str(),
        Some("shared"),
        "two members and nobody identified is the guest case; body: {body}"
    );
    for personal in [
        "calendar_today",
        "inbox_recent",
        "memory_recall",
        "routine_recall",
    ] {
        assert!(
            !offered(&body).contains(&personal.to_string()),
            "{personal} reached a screen nobody had identified themselves to; body: {body}"
        );
    }
}

/// The reason personal suggestions gate on not-Guest rather than on Owner.
#[tokio::test]
async fn one_member_is_the_personal_case_without_anyone_identifying_themselves() {
    let h = make_app(19).await;
    let _jerry = member(&h, "Jerry").await;

    let (status, body) = get_json(&h.app, "/api/v1/suggestions").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["audience"].as_str(),
        Some("personal"),
        "a household of one has exactly one possible answer to who is asking; body: {body}"
    );
}

/// Unlike `/proposals`, suggestions have no addressee to fail to resolve.
#[tokio::test]
async fn the_route_never_refuses_whoever_is_asking() {
    for members in 0..3 {
        let h = make_app(2).await;
        for i in 0..members {
            member(&h, &format!("m{i}")).await;
        }
        let (status, body) = get_json(&h.app, "/api/v1/suggestions?session_id=s-nobody").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "refused a caller on a pond with {members} members; body: {body}"
        );
    }
}

/// `Owner(them)` on a pond of two must not widen to `Household`, which reads every member.
#[tokio::test]
async fn an_identified_member_is_never_offered_another_members_composed_question() {
    let h = make_app(19).await;
    let jerry = member(&h, "Jerry").await;
    let liz = member(&h, "Liz").await;
    let lizs_question = "When is my appointment at the clinic?";
    compose_for(&h, &liz, "m-liz-clinic", lizs_question).await;

    let session = session_of(&h, "s-jerry", &jerry).await;
    let (status, body) =
        get_json(&h.app, &format!("/api/v1/suggestions?session_id={session}")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["audience"].as_str(),
        Some("personal"),
        "Jerry is identified, so this is his personal tier; body: {body}"
    );
    assert!(
        !prompts(&body).iter().any(|p| p == lizs_question),
        "Liz's composed question reached Jerry; body: {body}"
    );
}

/// Control: without it the test above passes if the composed tier is never served.
#[tokio::test]
async fn the_member_a_note_belongs_to_is_offered_the_question_composed_from_it() {
    let h = make_app(19).await;
    let _jerry = member(&h, "Jerry").await;
    let liz = member(&h, "Liz").await;
    let lizs_question = "When is my appointment at the clinic?";
    compose_for(&h, &liz, "m-liz-clinic", lizs_question).await;

    let session = session_of(&h, "s-liz", &liz).await;
    let (status, body) =
        get_json(&h.app, &format!("/api/v1/suggestions?session_id={session}")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        prompts(&body).iter().any(|p| p == lizs_question),
        "Liz was not offered the question composed from her own note; body: {body}"
    );
}

/// The template-tier multi-member test can't catch this: its suggestors are all silent here.
#[tokio::test]
async fn nobody_identified_on_a_pond_of_two_is_offered_no_composed_question() {
    let h = make_app(19).await;
    let _jerry = member(&h, "Jerry").await;
    let liz = member(&h, "Liz").await;
    compose_for(
        &h,
        &liz,
        "m-liz-clinic",
        "When is my appointment at the clinic?",
    )
    .await;

    let (status, body) = get_json(&h.app, "/api/v1/suggestions").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["audience"].as_str(), Some("shared"), "body: {body}");
    let composed: Vec<&Value> = body["suggestions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|s| s["composed"] == Value::Bool(true))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        composed.is_empty(),
        "a composed question reached the guest tier; body: {body}"
    );
}

/// Off must hide already-queued composed questions too; back on returns the same ones.
#[tokio::test]
async fn turning_composed_suggestions_off_takes_the_queued_ones_off_home() {
    use pond_core::user_data::ports::settings::SettingsRepository;

    let h = make_app(19).await;
    let jerry = member(&h, "Jerry").await;
    let question = "What did I decide about the garden fence?";
    compose_for(&h, &jerry, "m-jerry-fence", question).await;

    // Control: without it, "off" would pass on a harness that never serves the composed tier.
    let (_, body) = get_json(&h.app, "/api/v1/suggestions").await;
    assert!(
        prompts(&body).iter().any(|p| p == question),
        "on: body: {body}"
    );

    let mut off = h.settings.get().await.unwrap();
    off.suggestion_generation_enabled = false;
    h.settings.update(&off).await.unwrap();
    let (status, body) = get_json(&h.app, "/api/v1/suggestions").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        !prompts(&body).iter().any(|p| p == question),
        "off: a queued composed question is still on Home; body: {body}"
    );

    // Same queued row, not a rebuild: back on needn't wait for another composition pass.
    let mut on = h.settings.get().await.unwrap();
    on.suggestion_generation_enabled = true;
    h.settings.update(&on).await.unwrap();
    let (_, body) = get_json(&h.app, "/api/v1/suggestions").await;
    assert!(
        prompts(&body).iter().any(|p| p == question),
        "back on: body: {body}"
    );
}
