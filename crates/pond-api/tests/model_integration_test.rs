//! Model management routes. DELETE /api/v1/models/{category}/{name}: 404 when
//! uncatalogued, 409 when a role still holds it, 204 deletes and clears the flag.
//! POST the same path plus /activate: 400 on a category/role mismatch, 200 persists.
//! Run: cargo test -p pond-api --test model_integration_test

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
use pond_core::models::ports::model_repository::ModelRepository;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_model_repository::SqliteModelRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use tower::ServiceExt;

// ── Minimal stubs ──────────────────────────────────────────────────────────────

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

struct MockDeviceRegistry;

#[async_trait::async_trait]
impl DeviceRegistry for MockDeviceRegistry {
    async fn register(&self, req: RegisterDeviceRequest) -> anyhow::Result<Device> {
        Ok(Device {
            id: "mock".into(),
            name: req.name,
            device_type: req.device_type,
            hostname: req.hostname,
            ip_address: None,
            capabilities: req.capabilities,
            registered_at: "2024-01-01 00:00:00".into(),
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

// ── Fixture ────────────────────────────────────────────────────────────────────

/// Build a test router with a real SQLite model_repo backed by a tempdir.
async fn make_app() -> (
    axum::Router,
    Arc<dyn ModelRepository + Send + Sync>,
    tempfile::TempDir,
) {
    let (router, model_repo, _settings_repo, tmp) = make_app_with_settings_repo().await;
    (router, model_repo, tmp)
}

async fn make_app_with_settings_repo() -> (
    axum::Router,
    Arc<dyn ModelRepository + Send + Sync>,
    Arc<dyn SettingsRepository + Send + Sync>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();

    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));
    let model_repo: Arc<dyn ModelRepository + Send + Sync> =
        Arc::new(SqliteModelRepository::new(db.system.clone()));
    let settings_repo: Arc<dyn SettingsRepository + Send + Sync> =
        Arc::new(MockSettingsRepository::new());

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
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage,
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        tts_control: None,
        settings_repo: settings_repo.clone(),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(MockDeviceRegistry),
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
        model_repo: Some(model_repo.clone()),
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
        model_repo,
        settings_repo,
        tmp,
    )
}

fn gguf_record(name: &str) -> ModelRecord {
    ModelRecord {
        id: ModelRecord::id_for(&ModelCategory::Gguf, name),
        category: ModelCategory::Gguf,
        name: name.to_string(),
        filename: Some(format!("{name}.gguf")),
        description: format!("{name} model"),
        size_mb: 1000,
        url: Some("https://example.com/model.gguf".into()),
        hf_id: None,
        ram_estimate_mb: None,
        recommended_role: Some("chat".into()),
        context_length: None,
        quantization: None,
        asr_language: None,
        asr_size: None,
        tts_engine: None,
        tts_voice_name: None,
        config_filename: None,
        config_url: None,
        tts_url: None,
        sample_rate: None,
        downloaded: true,
        is_custom: false,
    }
}

fn whisper_record(name: &str) -> ModelRecord {
    ModelRecord {
        id: ModelRecord::id_for(&ModelCategory::Whisper, name),
        category: ModelCategory::Whisper,
        name: name.to_string(),
        filename: Some(format!("ggml-{name}.en.bin")),
        description: format!("Whisper {name}"),
        size_mb: 74,
        url: Some("https://example.com/whisper.bin".into()),
        hf_id: None,
        ram_estimate_mb: None,
        recommended_role: None,
        context_length: None,
        quantization: None,
        asr_language: Some("en".into()),
        asr_size: Some(name.to_string()),
        tts_engine: None,
        tts_voice_name: None,
        config_filename: None,
        config_url: None,
        tts_url: None,
        sample_rate: None,
        downloaded: true,
        is_custom: false,
    }
}

fn auth_req(method: &str, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", "Bearer test-token");
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let bytes = body
        .map(|b| serde_json::to_vec(&b).unwrap())
        .unwrap_or_default();
    builder.body(Body::from(bytes)).unwrap()
}

// ── DELETE /api/v1/models/{category}/{name} ────────────────────────────────────

#[tokio::test]
async fn delete_model_404_when_not_in_catalog() {
    let (app, _repo, _tmp) = make_app().await;
    let req = auth_req("DELETE", "/api/v1/models/gguf/nonexistent", None);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_model_409_when_model_has_active_role() {
    let (app, repo, _tmp) = make_app().await;

    let m = gguf_record("test-model");
    repo.upsert(&m).await.unwrap();
    repo.set_assignment("chat", "gguf/test-model")
        .await
        .unwrap();

    let req = auth_req("DELETE", "/api/v1/models/gguf/test-model", None);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn delete_model_204_clears_downloaded_flag() {
    let (app, repo, tmp) = make_app().await;

    let mut m = gguf_record("removable");
    // Create the actual file so the handler can delete it
    let model_dir = tmp.path().join("models").join("gguf");
    std::fs::create_dir_all(&model_dir).unwrap();
    let model_file = model_dir.join("removable.gguf");
    std::fs::write(&model_file, b"fake model data").unwrap();
    m.downloaded = true;
    repo.upsert(&m).await.unwrap();

    let req = auth_req("DELETE", "/api/v1/models/gguf/removable", None);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // File should be gone
    assert!(!model_file.exists(), "model file should have been deleted");

    // downloaded flag should be false
    let updated = repo.get_by_id("gguf/removable").await.unwrap().unwrap();
    assert!(!updated.downloaded, "downloaded flag should be cleared");
}

// ── POST /api/v1/models/{category}/{name}/activate ────────────────────────────

#[tokio::test]
async fn activate_model_400_when_role_category_mismatch() {
    let (app, repo, _tmp) = make_app().await;

    // Whisper model cannot be assigned to role "chat"
    repo.upsert(&whisper_record("base")).await.unwrap();
    let req = auth_req(
        "POST",
        "/api/v1/models/whisper/base/activate",
        Some(serde_json::json!({ "role": "chat" })),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn activate_model_404_when_not_in_catalog() {
    let (app, _repo, _tmp) = make_app().await;
    let req = auth_req(
        "POST",
        "/api/v1/models/gguf/ghost/activate",
        Some(serde_json::json!({ "role": "chat" })),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn activate_model_200_persists_assignment() {
    let (app, repo, _tmp) = make_app().await;

    repo.upsert(&gguf_record("llama-3b")).await.unwrap();
    let req = auth_req(
        "POST",
        "/api/v1/models/gguf/llama-3b/activate",
        Some(serde_json::json!({ "role": "chat" })),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let assignment = repo.get_assignment("chat").await.unwrap();
    assert!(assignment.is_some(), "assignment should be persisted");
    assert_eq!(assignment.unwrap().model_id, "gguf/llama-3b");
}

#[tokio::test]
async fn activate_gguf_model_sets_local_provider_in_settings() {
    let (app, repo, settings_repo, _tmp) = make_app_with_settings_repo().await;

    repo.upsert(&gguf_record("gemma-2b")).await.unwrap();
    let req = auth_req(
        "POST",
        "/api/v1/models/gguf/gemma-2b/activate",
        Some(serde_json::json!({ "role": "chat" })),
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let provider = settings_repo.get_key("chat_provider").await.unwrap();
    let model = settings_repo.get_key("chat_model").await.unwrap();
    assert_eq!(provider.as_deref(), Some("local"));
    assert_eq!(model.as_deref(), Some("gemma-2b"));
}

#[tokio::test]
async fn activate_whisper_model_for_asr_role() {
    let (app, repo, _tmp) = make_app().await;

    repo.upsert(&whisper_record("base")).await.unwrap();
    let req = auth_req(
        "POST",
        "/api/v1/models/whisper/base/activate",
        Some(serde_json::json!({ "role": "asr" })),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let assignment = repo.get_assignment("asr").await.unwrap();
    assert_eq!(assignment.unwrap().model_id, "whisper/base");
}

// ── Kokoro voices, reached through the "tts" group key ────────────────────────

fn kokoro_voice_record(name: &str) -> ModelRecord {
    ModelRecord {
        id: ModelRecord::id_for(&ModelCategory::TtsKokoro, name),
        category: ModelCategory::TtsKokoro,
        name: name.to_string(),
        filename: Some(format!("{name}.bin")),
        description: "American female, warm and unhurried".into(),
        size_mb: 1,
        url: Some("https://example.com/af_heart.bin".into()),
        hf_id: None,
        ram_estimate_mb: None,
        recommended_role: Some("tts".into()),
        context_length: None,
        quantization: None,
        asr_language: None,
        asr_size: None,
        tts_engine: Some("kokoro".into()),
        tts_voice_name: Some(name.to_string()),
        config_filename: None,
        config_url: None,
        tts_url: None,
        sample_rate: Some(24_000),
        downloaded: true,
        is_custom: false,
    }
}

/// The models list buckets every TTS engine under one `tts` group key, which is
/// not the record's own category. A lookup that uses the group key builds
/// `tts_piper/af_heart` and fails, so no Kokoro voice activates; Piper escapes
/// only because its group key and its category happen to be the same word.
#[tokio::test]
async fn activate_kokoro_voice_via_the_tts_group_key() {
    let (app, repo, _tmp) = make_app().await;
    repo.upsert(&kokoro_voice_record("af_heart")).await.unwrap();

    let req = auth_req(
        "POST",
        "/api/v1/models/tts/af_heart/activate",
        Some(serde_json::json!({ "role": "tts" })),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a Kokoro voice must be reachable through the group key the list endpoint hands out"
    );
}

/// The precise category must of course still work.
#[tokio::test]
async fn activate_kokoro_voice_via_its_own_category() {
    let (app, repo, _tmp) = make_app().await;
    repo.upsert(&kokoro_voice_record("bm_george"))
        .await
        .unwrap();

    let req = auth_req(
        "POST",
        "/api/v1/models/tts_kokoro/bm_george/activate",
        Some(serde_json::json!({ "role": "tts" })),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Forgiveness is scoped to TTS. A GGUF lookup must never resolve to some other
/// category's record that happens to share a name.
#[tokio::test]
async fn a_missing_non_tts_model_is_still_a_404() {
    let (app, repo, _tmp) = make_app().await;
    repo.upsert(&kokoro_voice_record("af_heart")).await.unwrap();

    let req = auth_req(
        "POST",
        "/api/v1/models/gguf/af_heart/activate",
        Some(serde_json::json!({ "role": "chat" })),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Activating a Kokoro voice must set the key the engine actually reads.
/// `active_tts_model` names the catalogue row; `voice_tts_voice` is what
/// `KokoroOutput` resolves `<voice>.bin` from and what the Voice screen shows.
/// Writing only the former leaves the picker and the spoken voice disagreeing.
#[tokio::test]
async fn activating_a_kokoro_voice_sets_voice_tts_voice() {
    let (app, repo, settings, _tmp) = make_app_with_settings_repo().await;
    repo.upsert(&kokoro_voice_record("bf_emma")).await.unwrap();

    // Start from the stale Piper spelling a real install carries.
    settings
        .set_key("voice_tts_voice", "en_US-ryan-high.onnx".into())
        .await
        .unwrap();

    let req = auth_req(
        "POST",
        "/api/v1/models/tts/bf_emma/activate",
        Some(serde_json::json!({ "role": "tts" })),
    );
    assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);

    // Only `voice_tts_voice` is asserted: `MockSettingsRepository` does not
    // project `active_tts_model` back out of its store, and that key was never
    // the broken one — it was already being written.
    let s = settings.get().await.unwrap();
    assert_eq!(s.voice_tts_voice, "bf_emma");
}
