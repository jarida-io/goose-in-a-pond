//! Model management routes (`/api/v1/models/{category}/{name}`) over a tempdir SQLite.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState, DownloadEntry};
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
    // Answering "not onboarded" would make every onboarding write route public.
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
    let f = pond_with(Arc::new(MockAgent::new())).await;
    (f.app, f.repo, f.settings, f.tmp)
}

type Tracker = Arc<tokio::sync::RwLock<std::collections::HashMap<String, DownloadEntry>>>;

/// Everything a test may need to reach behind the router.
struct Fixture {
    app: axum::Router,
    state: Arc<AppState>,
    repo: Arc<dyn ModelRepository + Send + Sync>,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
    tracker: Tracker,
    tmp: tempfile::TempDir,
}

async fn pond_with(agent: Arc<dyn pond_core::models::ports::agent::Agent>) -> Fixture {
    pond_with_catalog(agent, None).await
}

type Catalog =
    Option<Arc<dyn pond_core::models::ports::model_catalog_provider::ModelCatalogProvider>>;
type Scheduler = Option<Arc<dyn pond_core::models::ports::model_scheduler::ModelScheduler>>;

/// What a test may swap in behind the router.
#[derive(Default)]
struct Parts {
    catalog: Catalog,
    scheduler: Scheduler,
    /// The real settings store, for a test that reads `chat_provider` back through `get`.
    sqlite_settings: bool,
    /// A pond that started without a LiteRT-LM library.
    litert_library_missing: bool,
}

async fn pond_with_catalog(
    agent: Arc<dyn pond_core::models::ports::agent::Agent>,
    catalog: Catalog,
) -> Fixture {
    pond_with_parts(
        agent,
        Parts {
            catalog,
            ..Parts::default()
        },
    )
    .await
}

async fn pond_with_parts(
    agent: Arc<dyn pond_core::models::ports::agent::Agent>,
    parts: Parts,
) -> Fixture {
    let Parts {
        catalog,
        scheduler,
        sqlite_settings,
        litert_library_missing,
    } = parts;
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();

    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));
    let model_repo: Arc<dyn ModelRepository + Send + Sync> =
        Arc::new(SqliteModelRepository::new(db.system.clone()));
    let settings_repo: Arc<dyn SettingsRepository + Send + Sync> = if sqlite_settings {
        Arc::new(pond_infra::sqlite_settings::SqliteSettingsRepository::new(
            db.system.clone(),
        ))
    } else {
        Arc::new(MockSettingsRepository::new())
    };
    let tracker: Tracker = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));

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
        agent,
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
        engine_availability: pond_core::models::domain::engine::EngineAvailability {
            litert_library: !litert_library_missing,
        },
        scheduler: None,
        model_scheduler: scheduler,
        mcp_memory: None,
        extension_manager: None,
        mcp_server_repo: None,
        tool_registry: None,
        marketplace: None,
        secret_repo: None,
        download_tracker: tracker.clone(),
        piper_http_port: None,
        model_catalog_provider: catalog,
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
        setup_claims: None,
        spoken_codes: Default::default(),
        voice_samples: Default::default(),
    });

    Fixture {
        app: build_router(state.clone(), std::path::PathBuf::from("pond-desktop/dist")),
        state,
        repo: model_repo,
        settings: settings_repo,
        tracker,
        tmp,
    }
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
    assert_eq!(
        body_json(resp).await["error"],
        "test-model is doing a job right now (Conversation). Give that job to another model first."
    );
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

    assert!(!model_file.exists(), "model file should have been deleted");

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

/// The models list groups every TTS engine under `tts`, which is not a record's own category.
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

/// The group-key fallback is TTS-only; a GGUF lookup must never match another category.
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

/// `KokoroOutput` and the Voice screen read `voice_tts_voice`, not `active_tts_model`.
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

    // Only this key: `MockSettingsRepository` does not project `active_tts_model` back out.
    let s = settings.get().await.unwrap();
    assert_eq!(s.voice_tts_voice, "bf_emma");
}

// ── LiteRT-LM models ──────────────────────────────────────────────────────────

/// The id is the file name, extension included.
const LITERT: &str = "gemma-4-E2B-it.litertlm";

fn litert_record(name: &str) -> ModelRecord {
    ModelRecord {
        id: ModelRecord::id_for(&ModelCategory::Litert, name),
        category: ModelCategory::Litert,
        name: name.to_string(),
        filename: Some(name.to_string()),
        description: format!("{name} model"),
        size_mb: 2468,
        url: Some("https://example.com/model.litertlm".into()),
        hf_id: None,
        ram_estimate_mb: None,
        recommended_role: Some("chat".into()),
        context_length: Some(32768),
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

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// LiteRT-LM runs in the local provider; its assignment names the litert row.
#[tokio::test]
async fn activate_litert_model_sets_local_provider_in_settings() {
    let (app, repo, settings_repo, _tmp) = make_app_with_settings_repo().await;
    repo.upsert(&litert_record(LITERT)).await.unwrap();

    let req = auth_req(
        "POST",
        &format!("/api/v1/models/litert/{LITERT}/activate"),
        Some(serde_json::json!({ "role": "chat" })),
    );
    assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);

    let provider = settings_repo.get_key("chat_provider").await.unwrap();
    let model = settings_repo.get_key("chat_model").await.unwrap();
    assert_eq!(provider.as_deref(), Some("local"));
    assert_eq!(model.as_deref(), Some(LITERT));
    let assignment = repo.get_assignment("chat").await.unwrap().unwrap();
    assert_eq!(assignment.model_id, format!("litert/{LITERT}"));
}

/// Without the library a LiteRT-LM model is listed as unavailable, with the reason, and the pond
/// refuses to switch to it or fetch it rather than failing on the first turn.
#[tokio::test]
async fn without_the_litert_library_its_models_are_unavailable_and_refused() {
    let f = pond_with_parts(
        Arc::new(MockAgent::new()),
        Parts {
            litert_library_missing: true,
            ..Parts::default()
        },
    )
    .await;
    f.repo.upsert(&litert_record(LITERT)).await.unwrap();
    f.repo.upsert(&gguf_record("gemma-4-E2B")).await.unwrap();
    let not_here = ModelRecord {
        downloaded: false,
        ..litert_record("gemma-4-E4B-it.litertlm")
    };
    f.repo.upsert(&not_here).await.unwrap();

    let list = f
        .app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/models", None))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list = body_json(list).await;
    let litert = list["litert"].as_array().unwrap();
    assert_eq!(litert.len(), 2, "{list}");
    for row in litert {
        assert_eq!(row["available"], false, "{row}");
        assert_eq!(row["unavailable_reason"], "litert_library_missing", "{row}");
    }
    assert_eq!(list["gguf"][0]["available"], true, "{list}");
    assert!(
        list["gguf"][0].get("unavailable_reason").is_none(),
        "{list}"
    );

    let switch = f
        .app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/models/litert/{LITERT}/activate"),
            Some(serde_json::json!({ "role": "chat" })),
        ))
        .await
        .unwrap();
    assert_eq!(switch.status(), StatusCode::CONFLICT);
    let switch = body_json(switch).await;
    assert_eq!(switch["reason"], "litert_library_missing", "{switch}");
    let said = switch["error"].as_str().unwrap();
    assert!(said.contains("no LiteRT-LM library"), "{switch}");
    assert!(
        !said.contains("giap.sh"),
        "a shell command in household text: {switch}"
    );
    assert!(
        f.repo.get_assignment("chat").await.unwrap().is_none(),
        "a refused switch assigns nothing"
    );
    assert_eq!(f.settings.get_key("chat_model").await.unwrap(), None);

    let fetch = f
        .app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/models/litert/gemma-4-E4B-it.litertlm/download",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(fetch.status(), StatusCode::CONFLICT);
    assert!(f.tracker.read().await.is_empty(), "nothing was fetched");

    // Nor from a URL, and no row is left behind for it.
    let by_url = f
        .app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/models/download/url",
            Some(serde_json::json!({
                "url": "https://huggingface.co/a/b/resolve/main/gemma-4-E2B-it.litertlm",
                "category": "litert",
                "filename": "gemma-4-E2B-it.litertlm",
            })),
        ))
        .await
        .unwrap();
    assert_eq!(by_url.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(by_url).await["reason"], "litert_library_missing");
    assert!(f.tracker.read().await.is_empty(), "nothing was fetched");
    let listed = body_json(
        f.app
            .clone()
            .oneshot(auth_req("GET", "/api/v1/models", None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(listed["litert"].as_array().unwrap().len(), 2, "{listed}");

    // A model whose engine is here still switches.
    let gguf = f
        .app
        .oneshot(auth_req(
            "POST",
            "/api/v1/models/gguf/gemma-4-E2B/activate",
            Some(serde_json::json!({ "role": "chat" })),
        ))
        .await
        .unwrap();
    assert_eq!(gguf.status(), StatusCode::OK);
}

/// A chat model saved through settings must keep pointing at the litert row, not a gguf one.
#[tokio::test]
async fn saving_a_litert_chat_model_assigns_the_litert_row() {
    let (app, repo, _settings_repo, _tmp) = make_app_with_settings_repo().await;
    repo.upsert(&litert_record(LITERT)).await.unwrap();

    let req = auth_req(
        "PUT",
        "/api/v1/settings",
        Some(serde_json::json!({ "chat_provider": "local", "chat_model": LITERT })),
    );
    assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);

    let assignment = repo.get_assignment("chat").await.unwrap().unwrap();
    assert_eq!(assignment.model_id, format!("litert/{LITERT}"));
}

#[tokio::test]
async fn delete_litert_model_removes_its_file() {
    let (app, repo, tmp) = make_app().await;
    let dir = tmp.path().join("models").join("litertlm");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(LITERT);
    std::fs::write(&file, b"litertlm").unwrap();
    repo.upsert(&litert_record(LITERT)).await.unwrap();

    let req = auth_req("DELETE", &format!("/api/v1/models/litert/{LITERT}"), None);
    assert_eq!(
        app.oneshot(req).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    assert!(
        !file.exists(),
        "the .litertlm file should have been deleted"
    );
    let row = repo
        .get_by_id(&format!("litert/{LITERT}"))
        .await
        .unwrap()
        .unwrap();
    assert!(!row.downloaded);
}

/// A hand-copied file is found by a scan and listed in its own group, named by its file name.
#[tokio::test]
async fn a_litert_file_on_disk_is_listed_under_litert() {
    let (app, _repo, tmp) = make_app().await;
    let dir = tmp.path().join("models").join("litertlm");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(LITERT), b"litertlm").unwrap();

    let scan = app
        .clone()
        .oneshot(auth_req("POST", "/api/v1/models/scan", None))
        .await
        .unwrap();
    assert_eq!(scan.status(), StatusCode::OK);
    let scan = body_json(scan).await;
    assert_eq!(scan["found"], 1, "{scan}");
    assert_eq!(scan["entries"][0]["category"], "litert");
    assert_eq!(scan["entries"][0]["name"], LITERT);

    let list = app
        .oneshot(auth_req("GET", "/api/v1/models", None))
        .await
        .unwrap();
    let list = body_json(list).await;
    let litert = list["litert"].as_array().expect("a litert group");
    assert!(litert.iter().any(|m| m["name"] == LITERT), "{list}");
    let gguf = list["gguf"].as_array().cloned().unwrap_or_default();
    assert!(gguf.iter().all(|m| m["name"] != LITERT), "{list}");
}

// ── The disk scan keeps `downloaded` true to the disk ─────────────────────────

fn tracked(filename: &str, status: &str) -> DownloadEntry {
    let mut entry = DownloadEntry::starting(filename, "gguf");
    entry.status = status.to_string();
    entry
}

#[tokio::test]
async fn a_scan_corrects_stale_flags_but_not_a_file_mid_download() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let gguf = f.tmp.path().join("models").join("gguf");
    std::fs::create_dir_all(&gguf).unwrap();

    let mut gone = gguf_record("deleted-by-hand");
    gone.downloaded = true;
    f.repo.upsert(&gone).await.unwrap();

    let mut arrived = gguf_record("copied-by-hand");
    arrived.downloaded = false;
    std::fs::write(gguf.join("copied-by-hand.gguf"), b"weights").unwrap();
    f.repo.upsert(&arrived).await.unwrap();

    let mut fetching = gguf_record("coming-down");
    fetching.downloaded = true;
    f.repo.upsert(&fetching).await.unwrap();
    f.tracker.write().await.insert(
        "coming-down.gguf".into(),
        tracked("coming-down.gguf", "downloading"),
    );

    let resp = f
        .app
        .clone()
        .oneshot(auth_req("POST", "/api/v1/models/scan", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let flag = |id: &'static str| {
        let repo = f.repo.clone();
        async move { repo.get_by_id(id).await.unwrap().unwrap().downloaded }
    };
    assert!(!flag("gguf/deleted-by-hand").await, "its file is gone");
    assert!(flag("gguf/copied-by-hand").await, "its file is there");
    assert!(
        flag("gguf/coming-down").await,
        "a file with a live download is left to the download"
    );
}

// ── One pipeline acquires a model with its add-ons ────────────────────────────

/// Polls until `check` holds; a test that never gets there fails loudly after two seconds.
async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..100 {
        if check().await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn weights_server(file: &str) -> wiremock::MockServer {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(format!("/{file}")))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(b"GGUF weights".to_vec()))
        .mount(&server)
        .await;
    server
}

async fn post_json(
    app: &axum::Router,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(auth_req("POST", uri, body))
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

#[tokio::test]
async fn a_text_only_model_downloads_as_one_tracked_part_and_is_marked_downloaded() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let server = weights_server("my-model.gguf").await;
    let mut row = gguf_record("my-model");
    row.downloaded = false;
    row.url = Some(format!("{}/my-model.gguf", server.uri()));
    f.repo.upsert(&row).await.unwrap();

    let (status, body) = post_json(&f.app, "/api/v1/models/gguf/my-model/download", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "download_started");
    assert_eq!(body["pictures"], "text_only");
    assert_eq!(body["parts"][0]["part"], "model");
    assert_eq!(body["parts"].as_array().map(Vec::len), Some(1));
    assert!(body["message"]
        .as_str()
        .unwrap()
        .starts_with("Downloading my-model"));

    let repo = f.repo.clone();
    eventually("the row to read downloaded", || {
        let repo = repo.clone();
        async move {
            repo.get_by_id("gguf/my-model")
                .await
                .unwrap()
                .is_some_and(|m| m.downloaded)
        }
    })
    .await;
    assert!(f.tmp.path().join("models/gguf/my-model.gguf").exists());
    let tracker = f.tracker.read().await;
    let entry = tracker.get("my-model.gguf").expect("a tracker entry");
    assert_eq!(entry.status, "done");
    assert_eq!(entry.model_id.as_deref(), Some("gguf/my-model"));
    assert_eq!(entry.part.as_deref(), Some("model"));
}

/// The add-on is part of the download by default; unticked, only the model comes down.
#[tokio::test]
async fn leaving_pictures_out_fetches_only_the_model() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let name = "gemma-4-E2B-it-Q4_K_M";
    let server = weights_server(&format!("{name}.gguf")).await;
    let mut row = gguf_record(name);
    row.downloaded = false;
    row.url = Some(format!("{}/{name}.gguf", server.uri()));
    f.repo.upsert(&row).await.unwrap();

    let (status, body) = post_json(
        &f.app,
        &format!("/api/v1/models/gguf/{name}/download"),
        Some(serde_json::json!({"pictures": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parts = body["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["part"], "model");
    if !pond_core::models::domain::device_budget::budgeted_device() {
        assert_eq!(body["pictures"], "left_out");
    }
    let tracker = f.tracker.clone();
    eventually("the model part to finish", || {
        let tracker = tracker.clone();
        async move {
            tracker
                .read()
                .await
                .get(&format!("{name}.gguf"))
                .is_some_and(|e| e.status == "done")
        }
    })
    .await;
    assert_eq!(
        f.tracker.read().await.len(),
        1,
        "no picture part was registered"
    );
}

#[tokio::test]
async fn an_ollama_model_is_never_downloaded_by_the_pond() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let mut row = gguf_record("llama3.2:latest");
    row.id = ModelRecord::id_for(&ModelCategory::Ollama, "llama3.2:latest");
    row.category = ModelCategory::Ollama;
    row.filename = None;
    row.url = None;
    row.downloaded = false;
    f.repo.upsert(&row).await.unwrap();
    let (status, body) = post_json(
        &f.app,
        "/api/v1/models/ollama/llama3.2:latest/download",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "external");
}

/// A model whose size nothing has said is asked for it first: the parts and the announcement
/// carry the number before anything is fetched, and the row keeps it.
#[tokio::test]
async fn a_download_learns_an_unknown_size_before_it_starts() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let weights = vec![7u8; 3 * 1_048_576 + 5];
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("HEAD"))
        .and(wiremock::matchers::path("/sized-model.gguf"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-length", weights.len().to_string()),
        )
        .mount(&server)
        .await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/sized-model.gguf"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(weights.clone()))
        .mount(&server)
        .await;
    let mut row = gguf_record("sized-model");
    row.size_mb = 0;
    row.downloaded = false;
    row.url = Some(format!("{}/sized-model.gguf", server.uri()));
    f.repo.upsert(&row).await.unwrap();

    let (status, body) = post_json(
        &f.app,
        "/api/v1/models/gguf/sized-model/download",
        Some(serde_json::json!({"pictures": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["parts"][0]["size_bytes"], weights.len() as u64);
    assert_eq!(body["message"], "Downloading sized-model (3 MB)");
    let stored = f.repo.get_by_id("gguf/sized-model").await.unwrap().unwrap();
    assert_eq!(stored.size_mb, 3);
}

/// A file named by URL that the pairing table lists reads as the table names it.
#[tokio::test]
async fn a_listed_file_named_by_url_reads_as_the_table_names_it() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let (status, body) = post_json(
        &f.app,
        "/api/v1/models/download/url",
        Some(serde_json::json!({
            "url": "https://127.0.0.1:9/SmolVLM-256M-Instruct-Q8_0.gguf",
            "category": "gguf",
            "filename": "SmolVLM-256M-Instruct-Q8_0.gguf",
            "pictures": false,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("Downloading SmolVLM 256M"),
        "{body}"
    );
    let resp = f
        .app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/models", None))
        .await
        .unwrap();
    let list = body_json(resp).await;
    let row = list["gguf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "SmolVLM-256M-Instruct-Q8_0")
        .cloned()
        .unwrap();
    assert_eq!(row["title"], "SmolVLM 256M");
    assert_eq!(row["provenance"], "added");
}

/// A file named by URL becomes an Added row under its sanitised name, and a failure says why.
#[tokio::test]
async fn a_url_download_becomes_an_added_row_and_a_failure_keeps_its_reason() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let (status, body) = post_json(
        &f.app,
        "/api/v1/models/download/url",
        Some(serde_json::json!({
            "url": "https://127.0.0.1:9/nowhere.gguf",
            "category": "gguf",
            "filename": "../../escaped.gguf",
            "pictures": false,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "downloading");
    assert_eq!(body["filename"], "escaped.gguf");

    let row = f.repo.get_by_id("gguf/escaped").await.unwrap().unwrap();
    assert_eq!(row.filename.as_deref(), Some("escaped.gguf"));
    assert_eq!(row.url.as_deref(), Some("https://127.0.0.1:9/nowhere.gguf"));
    assert!(row.is_custom && !row.downloaded);

    let tracker = f.tracker.clone();
    eventually("the transfer to fail", || {
        let tracker = tracker.clone();
        async move {
            tracker
                .read()
                .await
                .get("escaped.gguf")
                .is_some_and(|e| e.status == "error" && e.error.is_some())
        }
    })
    .await;
    let reason = f.tracker.read().await["escaped.gguf"]
        .error
        .clone()
        .unwrap();
    assert!(reason.contains("could not be reached"), "{reason}");
    assert!(
        !reason.contains("127.0.0.1") && !reason.contains("http"),
        "the reason names no address: {reason}"
    );
    assert!(!f.tmp.path().join("escaped.gguf").exists());
    assert!(!f.tmp.path().join("models/gguf/escaped.gguf.part").exists());
}

#[tokio::test]
async fn pause_cancel_and_resume_keep_their_own_statuses() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let control =
        |filename: &str, action: &str| serde_json::json!({"filename": filename, "action": action});

    f.tracker
        .write()
        .await
        .insert("live.gguf".into(), tracked("live.gguf", "downloading"));
    let (status, body) = post_json(
        &f.app,
        "/api/v1/models/download/control",
        Some(control("live.gguf", "pause")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "pausing");
    assert_eq!(
        f.tracker.read().await["live.gguf"]
            .control
            .load(std::sync::atomic::Ordering::Relaxed),
        pond_api::DL_PAUSE
    );

    // A paused transfer has no task to see a flag: the route itself throws the partial away.
    let partial = f.tmp.path().join("waiting.gguf.incomplete");
    std::fs::write(&partial, b"half").unwrap();
    let mut paused = tracked("waiting.gguf", "paused");
    paused.partial = Some(partial.clone());
    f.tracker
        .write()
        .await
        .insert("waiting.gguf".into(), paused);
    let (status, body) = post_json(
        &f.app,
        "/api/v1/models/download/control",
        Some(control("waiting.gguf", "cancel")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "cancelled");
    assert!(!partial.exists(), "cancel deletes the partial file");
    assert_eq!(f.tracker.read().await["waiting.gguf"].status, "cancelled");

    // Resuming needs the recorded source.
    f.tracker.write().await.insert(
        "sourceless.gguf".into(),
        tracked("sourceless.gguf", "paused"),
    );
    let (status, _) = post_json(
        &f.app,
        "/api/v1/models/download/control",
        Some(control("sourceless.gguf", "resume")),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

const CONTROL: &str = "/api/v1/models/download/control";

/// A tracker entry for one part of a model.
fn part_of(filename: &str, model_id: &str, part: &str, status: &str) -> DownloadEntry {
    let mut entry = tracked(filename, status);
    entry.model_id = Some(model_id.to_string());
    entry.part = Some(part.to_string());
    entry
}

async fn flag(tracker: &Tracker, key: &str) -> u8 {
    tracker.read().await[key]
        .control
        .load(std::sync::atomic::Ordering::Relaxed)
}

fn filenames(body: &serde_json::Value) -> Vec<String> {
    body["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["filename"].as_str().map(str::to_string))
        .collect()
}

/// Cancelling a model's own file cancels its add-on, which is no use without it, and nothing of
/// another model's; cancelling an add-on alone leaves its model coming down.
#[tokio::test]
async fn cancelling_a_models_file_cancels_its_add_on_too() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    for (key, model, part) in [
        ("e2b.gguf", "gguf/e2b", "model"),
        ("mmproj/e2b/mmproj.gguf", "gguf/e2b", "pictures"),
        ("other.gguf", "gguf/other", "model"),
        ("mmproj/other/mmproj.gguf", "gguf/other", "pictures"),
    ] {
        f.tracker
            .write()
            .await
            .insert(key.into(), part_of(key, model, part, "downloading"));
    }

    let (status, body) = post_json(
        &f.app,
        CONTROL,
        Some(serde_json::json!({"filename": "mmproj/other/mmproj.gguf", "action": "cancel"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(filenames(&body), ["mmproj/other/mmproj.gguf"]);
    assert_eq!(flag(&f.tracker, "other.gguf").await, pond_api::DL_RUN);

    let (status, body) = post_json(
        &f.app,
        CONTROL,
        Some(serde_json::json!({"filename": "e2b.gguf", "action": "cancel"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "cancelling");
    assert_eq!(filenames(&body), ["e2b.gguf", "mmproj/e2b/mmproj.gguf"]);
    assert_eq!(flag(&f.tracker, "e2b.gguf").await, pond_api::DL_CANCEL);
    assert_eq!(
        flag(&f.tracker, "mmproj/e2b/mmproj.gguf").await,
        pond_api::DL_CANCEL
    );
    assert_eq!(flag(&f.tracker, "other.gguf").await, pond_api::DL_RUN);
}

/// A paused model and its paused add-on are both thrown away, partial files and all, by one
/// cancel of the model's file.
#[tokio::test]
async fn a_paused_models_add_on_goes_with_it() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let mut partials = Vec::new();
    for (key, part) in [
        ("e4b.gguf", "model"),
        ("mmproj/e4b/mmproj.gguf", "pictures"),
    ] {
        let partial = f.tmp.path().join(format!("{part}.incomplete"));
        std::fs::write(&partial, b"half").unwrap();
        let mut entry = part_of(key, "gguf/e4b", part, "paused");
        entry.partial = Some(partial.clone());
        partials.push(partial);
        f.tracker.write().await.insert(key.into(), entry);
    }
    let (status, body) = post_json(
        &f.app,
        CONTROL,
        Some(serde_json::json!({"filename": "e4b.gguf", "action": "cancel"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "cancelled");
    for key in ["e4b.gguf", "mmproj/e4b/mmproj.gguf"] {
        assert_eq!(f.tracker.read().await[key].status, "cancelled", "{key}");
    }
    assert!(
        partials.iter().all(|p| !p.exists()),
        "no partial file is left"
    );
}

/// `model_id` acts on every part of that model the action can move, and nothing else; a model
/// with nothing to move, or a request naming neither, is refused.
#[tokio::test]
async fn control_by_model_id_moves_every_part_it_can() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    for entry in [
        part_of("e2b.gguf", "gguf/e2b", "model", "downloading"),
        part_of("mmproj/e2b/mmproj.gguf", "gguf/e2b", "pictures", "done"),
        part_of("other.gguf", "gguf/other", "model", "downloading"),
    ] {
        f.tracker
            .write()
            .await
            .insert(entry.filename.clone(), entry);
    }
    let by_model = |action: &str| serde_json::json!({"model_id": "gguf/e2b", "action": action});

    let (status, body) = post_json(&f.app, CONTROL, Some(by_model("pause"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "pausing");
    assert_eq!(
        filenames(&body),
        ["e2b.gguf"],
        "a finished part is left alone"
    );
    assert_eq!(flag(&f.tracker, "e2b.gguf").await, pond_api::DL_PAUSE);
    assert_eq!(flag(&f.tracker, "other.gguf").await, pond_api::DL_RUN);

    f.tracker.write().await.get_mut("e2b.gguf").unwrap().status = "paused".into();
    let (status, body) = post_json(&f.app, CONTROL, Some(by_model("cancel"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "cancelled");
    assert_eq!(f.tracker.read().await["e2b.gguf"].status, "cancelled");

    let (status, _) = post_json(&f.app, CONTROL, Some(by_model("cancel"))).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "nothing of that model is left to cancel"
    );
    let (status, _) = post_json(
        &f.app,
        CONTROL,
        Some(serde_json::json!({"action": "pause"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = post_json(
        &f.app,
        CONTROL,
        Some(serde_json::json!({"model_id": "gguf/e2b", "action": "stop"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Resuming a model resumes each of its paused parts, and each arrives.
#[tokio::test]
async fn resuming_a_model_resumes_each_paused_part() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let server = wiremock::MockServer::start().await;
    for file in ["two-part.gguf", "two-part-mmproj.gguf"] {
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(format!("/{file}")))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(b"bytes".to_vec()))
            .mount(&server)
            .await;
    }
    for (file, part) in [
        ("two-part.gguf", "model"),
        ("two-part-mmproj.gguf", "pictures"),
    ] {
        let mut entry = part_of(file, "gguf/two-part", part, "paused");
        entry.url = Some(format!("{}/{file}", server.uri()));
        entry.dest = Some(f.tmp.path().join("models/gguf").join(file));
        f.tracker.write().await.insert(file.into(), entry);
    }
    let (status, body) = post_json(
        &f.app,
        CONTROL,
        Some(serde_json::json!({"model_id": "gguf/two-part", "action": "resume"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "resuming");
    assert_eq!(filenames(&body), ["two-part.gguf", "two-part-mmproj.gguf"]);
    let tracker = f.tracker.clone();
    eventually("both parts to arrive", || {
        let tracker = tracker.clone();
        async move {
            let t = tracker.read().await;
            ["two-part.gguf", "two-part-mmproj.gguf"]
                .iter()
                .all(|k| t[*k].status == "done")
        }
    })
    .await;
}

#[tokio::test]
async fn a_paused_download_is_never_evicted_and_a_finished_one_is() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let long_ago = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(600))
        .unwrap();
    let mut done = tracked("old.gguf", "done");
    done.finished_at = Some(long_ago);
    let mut paused = tracked("paused.gguf", "paused");
    paused.finished_at = Some(long_ago);
    f.tracker.write().await.insert("old.gguf".into(), done);
    f.tracker.write().await.insert("paused.gguf".into(), paused);

    let resp = f
        .app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/models/download/progress", None))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let names: Vec<&str> = body["downloads"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["filename"].as_str())
        .collect();
    assert_eq!(names, ["paused.gguf"]);
}

/// A live add-on part reads as downloading on its model's row.
#[tokio::test]
async fn the_pictures_companion_reads_downloading_while_its_part_is_fetched() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let name = "gemma-4-E2B-it-Q4_K_M";
    f.repo.upsert(&gguf_record(name)).await.unwrap();
    let companion = |list: &serde_json::Value| {
        list["gguf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == name)
            .map(|m| m["companions"][0].clone())
            .unwrap()
    };
    let list = |app: axum::Router| async move {
        let resp = app
            .oneshot(auth_req("GET", "/api/v1/models", None))
            .await
            .unwrap();
        body_json(resp).await
    };

    let before = companion(&list(f.app.clone()).await);
    assert_eq!(before["kind"], "pictures");
    assert_eq!(before["size_bytes"], 986_833_728u64);
    if pond_core::models::domain::device_budget::budgeted_device() {
        assert_eq!(before["state"], "not_on_this_device");
        return;
    }
    assert_eq!(before["state"], "available");

    let mut part = tracked("mmproj/gemma-4-e2b-it/mmproj-BF16.gguf", "downloading");
    part.model_id = Some(format!("gguf/{name}"));
    part.part = Some("pictures".into());
    f.tracker.write().await.insert(part.filename.clone(), part);
    assert_eq!(
        companion(&list(f.app.clone()).await)["state"],
        "downloading"
    );
}

#[tokio::test]
async fn the_explicit_pictures_route_refuses_a_litert_model() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    f.repo
        .upsert(&litert_record("gemma-4-E2B-it.litertlm"))
        .await
        .unwrap();
    let (status, body) = post_json(
        &f.app,
        "/api/v1/models/litert/gemma-4-E2B-it.litertlm/companions/pictures",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "text_only");
}

/// The boot restore brings back only an assigned model whose file is missing, and through the
/// tracker, so the household sees it coming down.
#[tokio::test]
async fn the_boot_restore_fetches_an_assigned_missing_model_through_the_tracker() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let server = weights_server("assigned.gguf").await;
    let mut assigned = gguf_record("assigned");
    assigned.downloaded = true;
    assigned.url = Some(format!("{}/assigned.gguf", server.uri()));
    f.repo.upsert(&assigned).await.unwrap();
    f.repo
        .set_assignment("chat", "gguf/assigned")
        .await
        .unwrap();

    let mut unassigned = gguf_record("bystander");
    unassigned.downloaded = false;
    unassigned.url = Some(format!("{}/bystander.gguf", server.uri()));
    f.repo.upsert(&unassigned).await.unwrap();

    let started = pond_api::model_acquisition::restore_assigned_models(f.state.clone()).await;
    assert_eq!(started, 1);
    assert!(f.tracker.read().await.contains_key("assigned.gguf"));
    assert!(
        !f.tracker.read().await.contains_key("bystander.gguf"),
        "nothing unassigned is fetched"
    );
    let tracker = f.tracker.clone();
    eventually("the restored model to arrive", || {
        let tracker = tracker.clone();
        async move {
            tracker
                .read()
                .await
                .get("assigned.gguf")
                .is_some_and(|e| e.status == "done")
        }
    })
    .await;
    assert!(f.tmp.path().join("models/gguf/assigned.gguf").exists());

    // Once it is there, a second restore has nothing to do.
    assert_eq!(
        pond_api::model_acquisition::restore_assigned_models(f.state.clone()).await,
        0
    );
}

/// Think and task select nothing yet, so a boot never fetches a missing model of theirs.
#[tokio::test]
async fn the_boot_restore_never_fetches_for_think_or_task() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let server = weights_server("qwen2.5-3b.gguf").await;
    for (name, role) in [("qwen2.5-3b", "think"), ("task-model", "task")] {
        let mut row = gguf_record(name);
        row.url = Some(format!("{}/{name}.gguf", server.uri()));
        f.repo.upsert(&row).await.unwrap();
        f.repo.set_assignment(role, &row.id).await.unwrap();
    }
    assert_eq!(
        pond_api::model_acquisition::restore_assigned_models(f.state.clone()).await,
        0
    );
    assert!(f.tracker.read().await.is_empty());
}

/// Another quant of an assigned model already on disk is the household's copy: no fetch at boot.
#[tokio::test]
async fn the_boot_restore_leaves_a_model_whose_other_quant_is_on_disk() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let server = weights_server("gemma-4-E4B-it-Q4_K_M.gguf").await;
    let gguf = f.tmp.path().join("models/gguf");
    std::fs::create_dir_all(&gguf).unwrap();
    std::fs::write(gguf.join("gemma-4-E4B-it-Q4_K_S.gguf"), b"weights").unwrap();
    let mut assigned = gguf_record("gemma-4-E4B-it-Q4_K_M");
    assigned.url = Some(format!("{}/gemma-4-E4B-it-Q4_K_M.gguf", server.uri()));
    f.repo.upsert(&assigned).await.unwrap();
    f.repo
        .set_assignment("chat", "gguf/gemma-4-E4B-it-Q4_K_M")
        .await
        .unwrap();

    assert_eq!(
        pond_api::model_acquisition::restore_assigned_models(f.state.clone()).await,
        0
    );
    assert!(f.tracker.read().await.is_empty());
    assert!(!gguf.join("gemma-4-E4B-it-Q4_K_M.gguf").exists());
}

/// Records what the routes hand the agent.
struct RecordingAgent {
    inner: MockAgent,
    prepared: std::sync::Mutex<Vec<String>>,
    forgotten: std::sync::Mutex<Vec<std::path::PathBuf>>,
}

impl Default for RecordingAgent {
    fn default() -> Self {
        Self {
            inner: MockAgent::new(),
            prepared: Default::default(),
            forgotten: Default::default(),
        }
    }
}

#[async_trait::async_trait]
impl pond_core::models::ports::agent::Agent for RecordingAgent {
    async fn chat(
        &self,
        request: pond_core::shared::domain::agent::AgentRequest,
    ) -> anyhow::Result<pond_core::shared::domain::agent::AgentResponse> {
        self.inner.chat(request).await
    }
    async fn chat_stream(
        &self,
        request: pond_core::shared::domain::agent::AgentRequest,
    ) -> anyhow::Result<
        futures::stream::BoxStream<
            'static,
            anyhow::Result<pond_core::shared::domain::agent::AgentStreamEvent>,
        >,
    > {
        self.inner.chat_stream(request).await
    }
    fn prepare_model(&self, model: &str) {
        self.prepared.lock().unwrap().push(model.to_string());
    }
    fn forget_model_file(&self, path: &std::path::Path) {
        self.forgotten.lock().unwrap().push(path.to_path_buf());
    }
}

/// An arrived file is registered through the agent; a deleted one is forgotten by it.
#[tokio::test]
async fn arrival_registers_through_the_agent_and_delete_forgets_the_file() {
    let agent = Arc::new(RecordingAgent::default());
    let f = pond_with(agent.clone()).await;
    let server = weights_server("fresh.gguf").await;
    let mut row = gguf_record("fresh");
    row.downloaded = false;
    row.url = Some(format!("{}/fresh.gguf", server.uri()));
    f.repo.upsert(&row).await.unwrap();

    let (status, body) = post_json(&f.app, "/api/v1/models/gguf/fresh/download", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let watched = agent.clone();
    eventually("the agent to be handed the model", || {
        let watched = watched.clone();
        async move { watched.prepared.lock().unwrap().as_slice() == ["fresh"] }
    })
    .await;

    let resp = f
        .app
        .clone()
        .oneshot(auth_req("DELETE", "/api/v1/models/gguf/fresh", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        agent.forgotten.lock().unwrap().as_slice(),
        [f.tmp.path().join("models/gguf/fresh.gguf")]
    );
}

/// A refresh prunes what nobody can use and keeps whatever is downloaded or assigned.
#[tokio::test]
async fn a_refresh_prunes_stale_rows_and_keeps_downloaded_and_assigned_ones() {
    let bundled = whisper_record("base");
    let catalog = Arc::new(
        pond_core::models::mocks::mock_model_catalog_provider::MockModelCatalogProvider::with_models(
            vec![bundled.clone()],
        ),
    );
    let f = pond_with_catalog(Arc::new(MockAgent::new()), Some(catalog)).await;
    let gguf = f.tmp.path().join("models/gguf");
    std::fs::create_dir_all(&gguf).unwrap();

    let mut retired = gguf_record("llama-3.2-3b");
    retired.downloaded = false;
    let mut kept_file = gguf_record("qwen2.5-3b");
    kept_file.downloaded = true;
    std::fs::write(gguf.join("qwen2.5-3b.gguf"), b"weights").unwrap();
    let mut assigned = gguf_record("gemma-2b");
    assigned.downloaded = false;
    let mut ghost = gguf_record("lost-by-hand");
    ghost.is_custom = true;
    ghost.downloaded = false;
    ghost.url = None;
    let mut added = gguf_record("added-by-url");
    added.is_custom = true;
    added.downloaded = false;
    let mut drafter = gguf_record("mtp-gemma-4-E2B-it");
    drafter.is_custom = true;
    drafter.recommended_role = Some("draft".into());
    for r in [&retired, &kept_file, &assigned, &ghost, &added, &drafter] {
        f.repo.upsert(r).await.unwrap();
    }
    f.repo
        .set_assignment("chat", "gguf/gemma-2b")
        .await
        .unwrap();

    let (status, body) = post_json(&f.app, "/api/v1/models/registry/refresh", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The prune deletes row by row, so wait for the last of them, not the first.
    let repo = f.repo.clone();
    eventually("the stale rows to be pruned", || {
        let repo = repo.clone();
        async move {
            let mut left = 0;
            for id in [
                "gguf/llama-3.2-3b",
                "gguf/lost-by-hand",
                "gguf/mtp-gemma-4-E2B-it",
            ] {
                left += usize::from(repo.get_by_id(id).await.unwrap().is_some());
            }
            left == 0
        }
    })
    .await;

    let ids: Vec<String> = f
        .repo
        .list_all()
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    for gone in [
        "gguf/llama-3.2-3b",
        "gguf/lost-by-hand",
        "gguf/mtp-gemma-4-E2B-it",
    ] {
        assert!(!ids.iter().any(|i| i == gone), "{gone} should be pruned");
    }
    for kept in [
        "gguf/qwen2.5-3b",
        "gguf/gemma-2b",
        "gguf/added-by-url",
        "whisper/base",
    ] {
        assert!(ids.iter().any(|i| i == kept), "{kept} should be kept");
    }
    assert!(
        gguf.join("qwen2.5-3b.gguf").exists(),
        "pruning never touches a file"
    );
}

/// An agent with no conversation model chosen.
struct NoModelAgent(MockAgent);

#[async_trait::async_trait]
impl pond_core::models::ports::agent::Agent for NoModelAgent {
    async fn chat(
        &self,
        request: pond_core::shared::domain::agent::AgentRequest,
    ) -> anyhow::Result<pond_core::shared::domain::agent::AgentResponse> {
        self.0.chat(request).await
    }
    async fn chat_stream(
        &self,
        request: pond_core::shared::domain::agent::AgentRequest,
    ) -> anyhow::Result<
        futures::stream::BoxStream<
            'static,
            anyhow::Result<pond_core::shared::domain::agent::AgentStreamEvent>,
        >,
    > {
        self.0.chat_stream(request).await
    }
    async fn ensure_conversation_model(
        &self,
    ) -> Result<(), pond_core::models::domain::conversation_model::NoConversationModel> {
        Err(pond_core::models::domain::conversation_model::NoConversationModel)
    }
}

/// With no model chosen every chat route answers `no_model` and saves nothing; nothing is
/// downloaded or picked in its place.
#[tokio::test]
async fn every_chat_route_answers_no_model_and_saves_nothing() {
    let f = pond_with(Arc::new(NoModelAgent(MockAgent::new()))).await;
    for (uri, body) in [
        (
            "/api/v1/chat",
            serde_json::json!({"message": "hello", "session_id": "no-model-a"}),
        ),
        (
            "/api/v1/chat/stream",
            serde_json::json!({"message": "hello", "session_id": "no-model-b"}),
        ),
        (
            "/api/v1/agent/chat/stream",
            serde_json::json!({"message": "hello", "session_id": "no-model-c"}),
        ),
    ] {
        let (status, reply) = post_json(&f.app, uri, Some(body)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{uri}: {reply}");
        assert_eq!(reply["code"], "no_model", "{uri}");
        assert!(
            reply["error"].as_str().unwrap().contains("Models page"),
            "{uri}"
        );
    }
    assert!(f.tracker.read().await.is_empty(), "nothing is downloaded");
    let sessions = f.state.session_storage.list_sessions().await.unwrap();
    assert!(
        sessions.iter().all(|s| !s.id.starts_with("no-model")),
        "no session was created for a refused turn"
    );
}

/// The Orin's reading with a model loaded: little free of the budget.
struct LoadedScheduler;

#[async_trait::async_trait]
impl pond_core::models::ports::model_scheduler::ModelScheduler for LoadedScheduler {
    async fn notify_wake_word(&self) {}
    fn memory_status(&self) -> pond_core::models::ports::model_scheduler::MemoryStatus {
        pond_core::models::ports::model_scheduler::MemoryStatus {
            total_mb: 7620,
            available_for_llm_mb: 1000,
            budget_mb: pond_core::models::domain::device_budget::llm_budget_mb(),
            loaded_model: None,
        }
    }
}

/// A 32 GB desktop's own reading with a model loaded.
struct DesktopScheduler;

#[async_trait::async_trait]
impl pond_core::models::ports::model_scheduler::ModelScheduler for DesktopScheduler {
    async fn notify_wake_word(&self) {}
    fn memory_status(&self) -> pond_core::models::ports::model_scheduler::MemoryStatus {
        pond_core::models::ports::model_scheduler::MemoryStatus {
            total_mb: 32_768,
            available_for_llm_mb: 22_000,
            budget_mb: 24_576,
            loaded_model: None,
        }
    }
}

/// What a switch away from the model in use would free is reported, from the file itself.
#[tokio::test]
async fn memory_status_counts_what_a_switch_would_free() {
    let f = pond_with_parts(
        Arc::new(MockAgent::new()),
        Parts {
            scheduler: Some(Arc::new(LoadedScheduler)),
            sqlite_settings: true,
            ..Parts::default()
        },
    )
    .await;
    let gguf = f.tmp.path().join("models/gguf");
    std::fs::create_dir_all(&gguf).unwrap();
    let file = std::fs::File::create(gguf.join("in-use.gguf")).unwrap();
    file.set_len(3000 * 1_048_576).unwrap();
    f.repo.upsert(&gguf_record("in-use")).await.unwrap();
    f.settings
        .set_key("chat_provider", "local".into())
        .await
        .unwrap();
    f.settings
        .set_key("chat_model", "in-use".into())
        .await
        .unwrap();

    let resp = f
        .app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/models/memory-status", None))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["available_for_llm_mb"], 1000);
    let budget = pond_core::models::domain::device_budget::llm_budget_mb();
    assert_eq!(body["budget_mb"], budget);
    assert_eq!(
        body["reclaimable_mb"],
        3000u64.min(budget.saturating_sub(1000))
    );

    // Another program's model frees nothing the pond holds.
    f.settings
        .set_key("chat_provider", "ollama".into())
        .await
        .unwrap();
    let resp = f
        .app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/models/memory-status", None))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["reclaimable_mb"], 0);
}

/// A desktop's own numbers come through whole, and what a switch frees is measured against its
/// own budget, not the board's.
#[tokio::test]
async fn memory_status_reports_a_desktop_by_its_own_budget() {
    let f = pond_with_parts(
        Arc::new(MockAgent::new()),
        Parts {
            scheduler: Some(Arc::new(DesktopScheduler)),
            sqlite_settings: true,
            ..Parts::default()
        },
    )
    .await;
    let gguf = f.tmp.path().join("models/gguf");
    std::fs::create_dir_all(&gguf).unwrap();
    let file = std::fs::File::create(gguf.join("in-use.gguf")).unwrap();
    file.set_len(4000 * 1_048_576).unwrap();
    f.repo.upsert(&gguf_record("in-use")).await.unwrap();
    for (key, value) in [("chat_provider", "local"), ("chat_model", "in-use")] {
        f.settings.set_key(key, value.into()).await.unwrap();
    }

    let resp = f
        .app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/models/memory-status", None))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["total_mb"], 32_768);
    assert_eq!(body["budget_mb"], 24_576);
    assert_eq!(body["available_for_llm_mb"], 22_000);
    assert_eq!(
        body["reclaimable_mb"], 2_576,
        "never more than the budget is short of"
    );
}

/// A model's download served slowly, so a second request lands while the first is in flight.
async fn slow_weights(file: &str, weights: &[u8]) -> wiremock::MockServer {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(format!("/{file}")))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_bytes(weights.to_vec())
                .set_delay(std::time::Duration::from_millis(300)),
        )
        .mount(&server)
        .await;
    server
}

async fn gets(server: &wiremock::MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "GET")
        .count()
}

/// Two requests for one download (two clients, or a click while a boot restore is fetching it)
/// are one transfer: the second joins the first rather than racing it for the same partial file,
/// which ended the finished download as an error.
#[tokio::test]
async fn asking_twice_for_a_download_in_flight_runs_it_once() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let weights = vec![9u8; 4 * 1_048_576];
    let server = slow_weights("twice.gguf", &weights).await;
    let mut row = gguf_record("twice");
    row.downloaded = false;
    row.url = Some(format!("{}/twice.gguf", server.uri()));
    f.repo.upsert(&row).await.unwrap();

    let uri = "/api/v1/models/gguf/twice/download";
    let ((first, a), (second, b)) =
        tokio::join!(post_json(&f.app, uri, None), post_json(&f.app, uri, None));
    assert_eq!((first, second), (StatusCode::OK, StatusCode::OK));
    let mut statuses = [a["status"].as_str().unwrap(), b["status"].as_str().unwrap()];
    statuses.sort();
    assert_eq!(statuses, ["already_downloading", "download_started"]);

    let tracker = f.tracker.clone();
    eventually("the transfer to end", || {
        let tracker = tracker.clone();
        async move {
            tracker
                .read()
                .await
                .get("twice.gguf")
                .is_some_and(|e| e.status != "downloading")
        }
    })
    .await;
    let entry = f.tracker.read().await.get("twice.gguf").cloned().unwrap();
    assert_eq!(entry.status, "done", "{:?}", entry.error);
    assert_eq!(gets(&server).await, 1, "one transfer, not two");
    assert_eq!(
        std::fs::read(f.tmp.path().join("models/gguf/twice.gguf")).unwrap(),
        weights
    );
    assert!(
        f.repo
            .get_by_id("gguf/twice")
            .await
            .unwrap()
            .unwrap()
            .downloaded
    );

    // Done is not running: asking again after it finished fetches again.
    f.repo.set_downloaded("gguf/twice", false).await.unwrap();
    let (_, again) = post_json(&f.app, uri, None).await;
    assert_eq!(again["status"], "download_started");
}

/// An entry left at downloading by nothing (a task that died) must not lock its file out.
#[tokio::test]
async fn a_download_left_at_downloading_by_nothing_can_be_asked_for_again() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let server = weights_server("stranded.gguf").await;
    let mut row = gguf_record("stranded");
    row.downloaded = false;
    row.url = Some(format!("{}/stranded.gguf", server.uri()));
    f.repo.upsert(&row).await.unwrap();
    let mut left = tracked("stranded.gguf", "downloading");
    left.control
        .store(pond_api::DL_PAUSE, std::sync::atomic::Ordering::Relaxed);
    f.tracker.write().await.insert("stranded.gguf".into(), left);

    let (status, body) = post_json(&f.app, "/api/v1/models/gguf/stranded/download", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "download_started");
    let tracker = f.tracker.clone();
    eventually("the transfer to arrive", || {
        let tracker = tracker.clone();
        async move {
            tracker
                .read()
                .await
                .get("stranded.gguf")
                .is_some_and(|e| e.status == "done")
        }
    })
    .await;
    assert!(f.tmp.path().join("models/gguf/stranded.gguf").exists());
}

/// Resuming a file whose transfer is running starts nothing, and says so.
#[tokio::test]
async fn resuming_a_download_that_is_running_starts_no_second_transfer() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let server = weights_server("running.gguf").await;
    let mut live = part_of("running.gguf", "gguf/running", "model", "downloading");
    live.url = Some(format!("{}/running.gguf", server.uri()));
    live.dest = Some(f.tmp.path().join("models/gguf/running.gguf"));
    // The transfer holds a clone of the flag for as long as it runs.
    let transfer = live.control.clone();
    f.tracker.write().await.insert("running.gguf".into(), live);

    let (status, body) = post_json(
        &f.app,
        CONTROL,
        Some(serde_json::json!({"filename": "running.gguf", "action": "resume"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "downloading");
    assert_eq!(gets(&server).await, 0, "nothing was started");
    assert_eq!(f.tracker.read().await["running.gguf"].status, "downloading");
    drop(transfer);
}

/// Two rows can name one file (an older row under another id). Deleting the one that is not in use
/// must not take the file from the one that is.
#[tokio::test]
async fn deleting_a_row_leaves_the_file_of_an_assigned_row_that_shares_it() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let gguf = f.tmp.path().join("models/gguf");
    std::fs::create_dir_all(&gguf).unwrap();
    std::fs::write(gguf.join("gemma-4-E4B-it-Q4_K_M.gguf"), b"weights").unwrap();
    let mut older = gguf_record("gemma-4-e4b");
    older.filename = Some("gemma-4-E4B-it-Q4_K_M.gguf".into());
    f.repo.upsert(&older).await.unwrap();
    let mut added = gguf_record("gemma-4-E4B-it-Q4_K_M");
    added.is_custom = true;
    f.repo.upsert(&added).await.unwrap();
    f.repo
        .set_assignment("chat", "gguf/gemma-4-e4b")
        .await
        .unwrap();

    let del = |name: &str| {
        let app = f.app.clone();
        let uri = format!("/api/v1/models/gguf/{name}");
        async move {
            let resp = app.oneshot(auth_req("DELETE", &uri, None)).await.unwrap();
            let status = resp.status();
            (status, body_json(resp).await)
        }
    };
    let (status, body) = del("gemma-4-E4B-it-Q4_K_M").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"],
        "Gemma 4 E4B uses the same file and is doing a job right now (Conversation). Give that \
         job to another model first.",
        "the other row by the name the household reads, and its job"
    );
    assert!(gguf.join("gemma-4-E4B-it-Q4_K_M.gguf").exists());
    assert!(
        f.repo
            .get_by_id("gguf/gemma-4-E4B-it-Q4_K_M")
            .await
            .unwrap()
            .unwrap()
            .downloaded
    );

    // Nothing in use shares it any more: the delete goes through.
    f.repo.delete("gguf/gemma-4-e4b").await.unwrap();
    f.repo
        .set_assignment("chat", "gguf/some-other-model")
        .await
        .unwrap();
    let resp = f
        .app
        .clone()
        .oneshot(auth_req(
            "DELETE",
            "/api/v1/models/gguf/gemma-4-E4B-it-Q4_K_M",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(!gguf.join("gemma-4-E4B-it-Q4_K_M.gguf").exists());
}

/// A host that refuses the size probe does not stop the download: it starts with no number named
/// in the announcement and arrives all the same.
#[tokio::test]
async fn a_download_whose_host_refuses_a_head_still_starts_and_arrives() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let server = weights_server("no-head.gguf").await;
    wiremock::Mock::given(wiremock::matchers::method("HEAD"))
        .respond_with(wiremock::ResponseTemplate::new(405))
        .mount(&server)
        .await;
    let mut row = gguf_record("no-head");
    row.size_mb = 0;
    row.downloaded = false;
    row.url = Some(format!("{}/no-head.gguf", server.uri()));
    f.repo.upsert(&row).await.unwrap();

    let (status, body) = post_json(&f.app, "/api/v1/models/gguf/no-head/download", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["message"], "Downloading no-head");
    assert!(body["parts"][0]["size_bytes"].is_null());
    let repo = f.repo.clone();
    eventually("the model to arrive", || {
        let repo = repo.clone();
        async move {
            repo.get_by_id("gguf/no-head")
                .await
                .unwrap()
                .is_some_and(|m| m.downloaded)
        }
    })
    .await;
}

/// While a host takes its time to state a size, the row can change; recording the size must not
/// put back what the row was when the request began.
#[tokio::test]
async fn learning_a_size_does_not_undo_what_the_row_became_meanwhile() {
    let f = pond_with(Arc::new(MockAgent::new())).await;
    let weights = vec![5u8; 3 * 1_048_576];
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("HEAD"))
        .and(wiremock::matchers::path("/slow-head.gguf"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-length", weights.len().to_string())
                .set_delay(std::time::Duration::from_millis(400)),
        )
        .mount(&server)
        .await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/slow-head.gguf"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_bytes(weights.clone())
                .set_delay(std::time::Duration::from_secs(3)),
        )
        .mount(&server)
        .await;
    let mut row = gguf_record("slow-head");
    row.size_mb = 0;
    row.downloaded = false;
    row.url = Some(format!("{}/slow-head.gguf", server.uri()));
    f.repo.upsert(&row).await.unwrap();

    let app = f.app.clone();
    let request = tokio::spawn(async move {
        post_json(&app, "/api/v1/models/gguf/slow-head/download", None).await
    });
    let host = &server;
    eventually("the size probe to reach the host", || async move {
        host.received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.method.as_str() == "HEAD")
    })
    .await;
    // Another client changes the row while the probe waits.
    f.repo.set_downloaded("gguf/slow-head", true).await.unwrap();

    let (status, body) = request.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let after = f.repo.get_by_id("gguf/slow-head").await.unwrap().unwrap();
    assert_eq!(after.size_mb, 3, "the size learned is kept");
    assert!(
        after.downloaded,
        "and the change made meanwhile is not undone"
    );
}
