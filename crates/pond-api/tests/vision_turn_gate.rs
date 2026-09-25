//! Picture support at the HTTP edge (design_v2 section F), driven through the real router.
//!
//! The refusals are the point of most of this file: an image turn the active model cannot take
//! must be a real 409 or 415 BEFORE anything is saved, because a refused turn that left its
//! question in the history with no answer under it is what the desktop's draft restore exists to
//! avoid. So every refusal here is checked against the session store, not just the status.
//! Run: cargo test -p pond-api --test vision_turn_gate

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use futures::stream::BoxStream;
use pond_api::{build_router, AppState};
use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
use pond_core::models::domain::vision_encoder::EncoderState;
use pond_core::models::ports::agent::{Agent, WarmupPhase};
use pond_core::models::ports::model_repository::ModelRepository;
use pond_core::shared::domain::agent::{AgentRequest, AgentResponse, AgentStreamEvent};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_model_repository::SqliteModelRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_settings::SqliteSettingsRepository;
use reqwest::Client as ReqwestClient;
use serde_json::{json, Value};
use tower::ServiceExt;

const E2B: &str = "gemma-4-E2B-it-Q4_K_M";
const E2B_ENCODER_BYTES: u64 = 986_833_728;

// ── An agent that reports picture support and records what it was asked ────────

/// Answers `vision_state` from a per-model table (a model not in it is unknown), and records
/// every question, every `prepare_model`, every warm-up and every turn that reached it.
struct VisionAgent {
    inner: MockAgent,
    table: Mutex<HashMap<String, EncoderState>>,
    asked: Mutex<Vec<(String, String)>>,
    prepared: Mutex<Vec<String>>,
    prewarms: AtomicUsize,
    turns: AtomicUsize,
}

impl Default for VisionAgent {
    fn default() -> Self {
        Self {
            inner: MockAgent::new(),
            table: Mutex::default(),
            asked: Mutex::default(),
            prepared: Mutex::default(),
            prewarms: AtomicUsize::new(0),
            turns: AtomicUsize::new(0),
        }
    }
}

impl VisionAgent {
    fn with(model: &str, state: EncoderState) -> Arc<Self> {
        let agent = Self::default();
        agent.set(model, state);
        Arc::new(agent)
    }
    fn set(&self, model: &str, state: EncoderState) {
        self.table.lock().unwrap().insert(model.to_string(), state);
    }
    fn prepared(&self) -> Vec<String> {
        self.prepared.lock().unwrap().clone()
    }
    fn asked(&self) -> Vec<(String, String)> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Agent for VisionAgent {
    async fn chat(&self, request: AgentRequest) -> anyhow::Result<AgentResponse> {
        self.inner.chat(request).await
    }

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<AgentStreamEvent>>> {
        self.turns.fetch_add(1, Ordering::SeqCst);
        self.inner.chat_stream(request).await
    }

    fn vision_state(&self, provider: &str, model: &str) -> Option<EncoderState> {
        self.asked
            .lock()
            .unwrap()
            .push((provider.to_string(), model.to_string()));
        self.table.lock().unwrap().get(model).cloned()
    }

    fn prepare_model(&self, model: &str) {
        self.prepared.lock().unwrap().push(model.to_string());
    }

    async fn prewarm(&self, _voice_mode: bool, progress: Arc<dyn Fn(WarmupPhase) + Send + Sync>) {
        self.prewarms.fetch_add(1, Ordering::SeqCst);
        progress(WarmupPhase::Skipped {
            reason: "test agent".to_string(),
        });
    }
}

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
    async fn is_complete(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

struct MockDeviceRegistry;

#[async_trait::async_trait]
impl DeviceRegistry for MockDeviceRegistry {
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

// ── Fixture ────────────────────────────────────────────────────────────────────

struct Pond {
    app: axum::Router,
    agent: Arc<VisionAgent>,
    sessions: Arc<SqliteSessionStorage>,
    settings: Arc<SqliteSettingsRepository>,
    models: Arc<dyn ModelRepository + Send + Sync>,
    tmp: tempfile::TempDir,
}

/// A router over a real tempdir database (sessions, settings and models are all SQLite, so
/// "nothing was saved" is asked of the store the pond really writes), with `chat_provider` and
/// `chat_model` set the way activating a GGUF sets them.
async fn pond(agent: Arc<VisionAgent>, provider: &str, model: &str) -> Pond {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    // Attachments into the tempdir: the default is the real app-support directory, and these
    // turns carry pictures.
    let sessions = Arc::new(
        SqliteSessionStorage::new(db.system.clone())
            .with_attachment_dir(tmp.path().join("attachments")),
    );
    let settings = Arc::new(SqliteSettingsRepository::new(db.system.clone()));
    settings
        .set_key("chat_provider", provider.to_string())
        .await
        .unwrap();
    settings
        .set_key("chat_model", model.to_string())
        .await
        .unwrap();
    let models: Arc<dyn ModelRepository + Send + Sync> =
        Arc::new(SqliteModelRepository::new(db.system.clone()));

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
        session_storage: sessions.clone(),
        http_client: ReqwestClient::new(),
        agent: agent.clone(),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        tts_control: None,
        settings_repo: settings.clone(),
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
        model_repo: Some(models.clone()),
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
        mcp_app_resources: HashMap::new(),
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
    Pond {
        app: build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        agent,
        sessions,
        settings,
        models,
        tmp,
    }
}

fn request(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
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

async fn send(pond: &Pond, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = pond.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

async fn send_json(pond: &Pond, req: Request<Body>) -> (StatusCode, Value) {
    let (status, bytes) = send(pond, req).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn jpeg() -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut out)
        .encode(
            &[10, 20, 30, 40, 50, 60],
            2,
            1,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    out
}

fn webp(width: u32, height: u32) -> Vec<u8> {
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x * 9) as u8, (y * 3) as u8, 200])
    }));
    let mut out = Vec::new();
    img.write_with_encoder(image::codecs::webp::WebPEncoder::new_lossless(&mut out))
        .unwrap();
    out
}

fn image_turn(session_id: &str, data: &[u8], mime: &str) -> Value {
    json!({
        "session_id": session_id,
        "message": "what is in this photo?",
        "images": [{"data": b64(data), "mime_type": mime}],
    })
}

/// Nothing at all was written for `session_id`: no session row, so no message rows either.
async fn assert_nothing_saved(pond: &Pond, session_id: &str) {
    assert!(
        pond.sessions.get_session(session_id).await.is_err(),
        "a refused turn must not create its session"
    );
    let messages = pond
        .sessions
        .get_messages(session_id)
        .await
        .unwrap_or_default();
    assert!(messages.is_empty(), "a refused turn saved {messages:?}");
    assert_eq!(
        pond.agent.turns.load(Ordering::SeqCst),
        0,
        "a refused turn must never reach the engine"
    );
}

/// Wait for a background effect, without a fixed sleep that is either too short on a busy CI
/// runner or wasted everywhere else.
async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

fn gguf_row(name: &str, downloaded: bool, url: Option<String>) -> ModelRecord {
    ModelRecord {
        id: ModelRecord::id_for(&ModelCategory::Gguf, name),
        category: ModelCategory::Gguf,
        name: name.to_string(),
        filename: Some(format!("{name}.gguf")),
        description: format!("{name} model"),
        size_mb: 2900,
        url,
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
        downloaded,
        is_custom: false,
    }
}

// ── The 409s: refused before anything is saved ─────────────────────────────────

#[tokio::test]
async fn a_text_only_model_refuses_a_picture_with_409_and_saves_nothing() {
    let agent = VisionAgent::with("Llama-3.2-3B", EncoderState::NotDeclared);
    let pond = pond(agent, "local", "Llama-3.2-3B").await;

    let (status, body) = send_json(
        &pond,
        request(
            "POST",
            "/api/v1/chat/stream",
            Some(image_turn("refused-unsupported", &jpeg(), "image/jpeg")),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vision_unsupported");
    assert_eq!(body["state"], json!({"kind": "not_declared"}));
    assert_eq!(
        body["error"],
        "This model cannot look at pictures. To send one, choose a model marked Reads pictures on \
         the Models page."
    );
    // The handler asked about the model the settings name, under their provider.
    assert_eq!(
        pond.agent.asked(),
        vec![("local".to_string(), "Llama-3.2-3B".to_string())]
    );
    assert_nothing_saved(&pond, "refused-unsupported").await;
}

#[tokio::test]
async fn a_resumable_turn_is_refused_before_it_is_registered_as_a_run() {
    let agent = VisionAgent::with(E2B, EncoderState::Absent);
    let pond = pond(agent, "local", E2B).await;

    let mut turn = image_turn("refused-resumable", &jpeg(), "image/jpeg");
    turn["resumable"] = json!(true);
    let (status, body) = send_json(&pond, request("POST", "/api/v1/chat/stream", Some(turn))).await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vision_not_ready");
    assert_eq!(body["state"]["kind"], "absent");
    assert_nothing_saved(&pond, "refused-resumable").await;

    // No run was left registered against the session either.
    let (status, _) = send(
        &pond,
        request("GET", "/api/v1/sessions/refused-resumable/active-run", None),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "a refused turn must not register a run"
    );
}

#[tokio::test]
async fn the_agent_route_refuses_a_picture_while_support_downloads_and_saves_nothing() {
    let agent = VisionAgent::with(
        E2B,
        EncoderState::Downloading {
            done: 100 * 1_048_576,
            total: E2B_ENCODER_BYTES,
        },
    );
    let pond = pond(agent, "local", E2B).await;

    let (status, body) = send_json(
        &pond,
        request(
            "POST",
            "/api/v1/agent/chat/stream",
            Some(image_turn("refused-agent", &jpeg(), "image/jpeg")),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vision_not_ready");
    assert_eq!(
        body["error"],
        "Getting picture support ready: 100 MB of 941 MB. Text chat works meanwhile."
    );
    assert_nothing_saved(&pond, "refused-agent").await;
}

#[tokio::test]
async fn another_pond_refuses_pictures_with_the_mesh_line() {
    let agent = VisionAgent::with(E2B, EncoderState::NotDeclared);
    let pond = pond(agent, "mesh", E2B).await;

    let (status, body) = send_json(
        &pond,
        request(
            "POST",
            "/api/v1/chat/stream",
            Some(image_turn("refused-mesh", &jpeg(), "image/jpeg")),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "vision_unsupported");
    assert_eq!(
        body["error"],
        "Pictures cannot be sent to another pond yet. Switch back to a model on this device to \
         send one."
    );
    assert_nothing_saved(&pond, "refused-mesh").await;
}

#[tokio::test]
async fn an_agent_that_does_not_report_passes_the_picture_through() {
    // Nothing in the table: `vision_state` is None, which fails open to the adapter.
    let pond = pond(Arc::new(VisionAgent::default()), "local", E2B).await;
    let (status, _) = send(
        &pond,
        request(
            "POST",
            "/api/v1/chat/stream",
            Some(image_turn("passes-unknown", &jpeg(), "image/jpeg")),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pond.agent.turns.load(Ordering::SeqCst), 1);
    let sent = pond.agent.inner.last_request().unwrap();
    assert_eq!(sent.images.len(), 1);
    assert_eq!(
        sent.images[0].data,
        b64(&jpeg()),
        "a JPEG reaches the engine as sent"
    );
}

#[tokio::test]
async fn a_text_turn_never_asks_about_picture_support() {
    let agent = VisionAgent::with("Llama-3.2-3B", EncoderState::NotDeclared);
    let pond = pond(agent, "local", "Llama-3.2-3B").await;
    let (status, _) = send(
        &pond,
        request(
            "POST",
            "/api/v1/chat/stream",
            Some(json!({"session_id": "text-only", "message": "hello"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        pond.agent.asked().is_empty(),
        "a text turn must not pay for a picture check"
    );
}

// ── The 415 and the WebP re-encode ──────────────────────────────────────────────

#[tokio::test]
async fn garbage_behind_a_webp_header_is_a_415_and_saves_nothing() {
    let agent = VisionAgent::with(E2B, EncoderState::Ready { bytes: Some(1) });
    let pond = pond(agent, "local", E2B).await;

    let mut turn = image_turn("unreadable", &jpeg(), "image/jpeg");
    turn["images"]
        .as_array_mut()
        .unwrap()
        .push(json!({"data": b64(b"RIFF\x24\x00\x00\x00WEBPVP8 garbage, not a picture"), "mime_type": "image/webp"}));
    let (status, body) = send_json(&pond, request("POST", "/api/v1/chat/stream", Some(turn))).await;

    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
    assert_eq!(body["code"], "image_unreadable");
    assert_eq!(
        body["error"],
        "Picture 2 could not be read. Save it as a JPEG or PNG and attach it again."
    );
    assert_nothing_saved(&pond, "unreadable").await;
}

#[tokio::test]
async fn a_webp_reaches_the_engine_as_a_jpeg_and_is_saved_as_one() {
    let agent = VisionAgent::with(E2B, EncoderState::Ready { bytes: Some(1) });
    let pond = pond(agent, "local", E2B).await;

    // Labelled image/jpeg on purpose: the label is exactly what cannot be trusted.
    let (status, _) = send(
        &pond,
        request(
            "POST",
            "/api/v1/chat/stream",
            Some(image_turn("transcoded", &webp(32, 24), "image/jpeg")),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let sent = pond.agent.inner.last_request().unwrap();
    assert_eq!(sent.images[0].mime_type, "image/jpeg");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&sent.images[0].data)
        .unwrap();
    assert_eq!(
        &bytes[..3],
        &[0xFF, 0xD8, 0xFF],
        "the engine got a real JPEG"
    );
    let back = image::load_from_memory(&bytes).unwrap();
    assert_eq!((back.width(), back.height()), (32, 24));

    // The stored turn carries the converted picture too, so a later turn replays a JPEG.
    let stored = pond
        .sessions
        .list_session_attachments("transcoded")
        .await
        .unwrap();
    assert_eq!(stored.len(), 1, "the picture was saved with the turn");
    assert_eq!(stored[0].mime_type, "image/jpeg");
}

// ── GET /models/vision-status ──────────────────────────────────────────────────

#[tokio::test]
async fn vision_status_reports_the_active_model_in_the_documented_shape() {
    let agent = VisionAgent::with(
        E2B,
        EncoderState::Downloading {
            done: 412 * 1_048_576,
            total: E2B_ENCODER_BYTES,
        },
    );
    let pond = pond(agent, "local", E2B).await;

    let (status, body) =
        send_json(&pond, request("GET", "/api/v1/models/vision-status", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "model": E2B,
            "state": {"kind": "downloading", "done": 412 * 1_048_576, "total": E2B_ENCODER_BYTES},
            "size_bytes": E2B_ENCODER_BYTES,
            "message": "Getting picture support ready: 412 MB of 941 MB. Text chat works meanwhile.",
        })
    );
    assert_eq!(
        pond.agent.asked(),
        vec![("local".to_string(), E2B.to_string())]
    );
    assert!(
        pond.agent.prepared().is_empty(),
        "reading the status must never start anything"
    );

    pond.agent.set(
        E2B,
        EncoderState::Ready {
            bytes: Some(E2B_ENCODER_BYTES),
        },
    );
    let (_, body) = send_json(&pond, request("GET", "/api/v1/models/vision-status", None)).await;
    assert_eq!(
        body["state"],
        json!({"kind": "ready", "bytes": E2B_ENCODER_BYTES})
    );
    assert_eq!(body["message"], Value::Null);
    assert_eq!(body["size_bytes"], E2B_ENCODER_BYTES);
}

#[tokio::test]
async fn vision_status_is_unknown_with_nothing_to_say_when_the_agent_does_not_report() {
    let pond = pond(Arc::new(VisionAgent::default()), "local", E2B).await;
    let (status, body) =
        send_json(&pond, request("GET", "/api/v1/models/vision-status", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"model": E2B, "state": {"kind": "unknown"}, "size_bytes": null, "message": null})
    );
}

// ── GET /models: the static fact per GGUF row ──────────────────────────────────

#[tokio::test]
async fn the_model_list_says_which_gguf_rows_read_pictures_and_what_that_costs() {
    let agent = Arc::new(VisionAgent::default());
    agent.set(E2B, EncoderState::Absent);
    agent.set("Llama-3.2-3B", EncoderState::NotDeclared);
    agent.set("gemma-4-E4B-it-IQ4_XS", EncoderState::NotOnThisDevice);
    let pond = pond(agent, "local", E2B).await;
    for row in [
        gguf_row(E2B, true, None),
        gguf_row("Llama-3.2-3B", true, None),
        gguf_row("gemma-4-E4B-it-IQ4_XS", false, None),
        // The agent has no verdict here: pond-core's pinned table decides.
        gguf_row("gemma-4-E4B-it-Q4_K_M", false, None),
    ] {
        pond.models.upsert(&row).await.unwrap();
    }

    let (status, body) = send_json(&pond, request("GET", "/api/v1/models", None)).await;
    assert_eq!(status, StatusCode::OK);
    let row = |name: &str| -> Value {
        body["gguf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name} missing from {body}"))
    };

    assert_eq!(row(E2B)["reads_images"], true);
    assert_eq!(row(E2B)["image_support_bytes"], E2B_ENCODER_BYTES);
    assert_eq!(row("Llama-3.2-3B")["reads_images"], false);
    assert!(row("Llama-3.2-3B").get("image_support_bytes").is_none());
    assert_eq!(row("gemma-4-E4B-it-IQ4_XS")["reads_images"], false);
    assert_eq!(row("gemma-4-E4B-it-Q4_K_M")["reads_images"], true);
    assert_eq!(
        row("gemma-4-E4B-it-Q4_K_M")["image_support_bytes"],
        991_552_320u64
    );
    assert!(
        pond.agent.prepared().is_empty(),
        "listing models must never start a download"
    );
}

// ── Triggers: a model that arrived or was chosen gets prepared ─────────────────

#[tokio::test]
async fn activating_a_chat_model_prepares_it_and_warms_once() {
    let pond = pond(Arc::new(VisionAgent::default()), "local", "Llama-3.2-3B").await;
    pond.models
        .upsert(&gguf_row(E2B, true, None))
        .await
        .unwrap();

    let (status, _) = send(
        &pond,
        request(
            "POST",
            &format!("/api/v1/models/gguf/{E2B}/activate"),
            Some(json!({"role": "chat"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pond.agent.prepared(), vec![E2B.to_string()]);
    let agent = pond.agent.clone();
    eventually("the activation warm-up", || {
        agent.prewarms.load(Ordering::SeqCst) == 1
    })
    .await;

    // "Use" on the model already in use is not a change: prepared again (cheap, idempotent in
    // the adapter), but no second warm-up.
    let (status, _) = send(
        &pond,
        request(
            "POST",
            &format!("/api/v1/models/gguf/{E2B}/activate"),
            Some(json!({"role": "chat"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(pond.agent.prewarms.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_finished_gguf_download_prepares_the_model() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(format!("/{E2B}.gguf")))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(b"GGUF weights".to_vec()))
        .mount(&server)
        .await;

    let pond = pond(Arc::new(VisionAgent::default()), "local", "Llama-3.2-3B").await;
    pond.models
        .upsert(&gguf_row(
            E2B,
            false,
            Some(format!("{}/{E2B}.gguf", server.uri())),
        ))
        .await
        .unwrap();

    let (status, body) = send_json(
        &pond,
        request("POST", &format!("/api/v1/models/gguf/{E2B}/download"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let agent = pond.agent.clone();
    eventually("prepare_model after the download", || {
        agent.prepared() == vec![E2B.to_string()]
    })
    .await;
    assert!(pond
        .tmp
        .path()
        .join("models/gguf")
        .join(format!("{E2B}.gguf"))
        .exists());
}

// ── PUT /settings: a new chat model ────────────────────────────────────────────

/// A save that changes the engine's model starts ONE warm-up and hands the model to
/// `prepare_model`; re-sending the same value is no change and puts no Warming banner up; and an
/// Ollama tag that names a Gemma family is not a local GGUF, so there is nothing to provision.
#[tokio::test]
async fn a_new_chat_model_warms_once_and_only_a_local_one_is_prepared() {
    let pond = pond(Arc::new(VisionAgent::default()), "local", E2B).await;
    let put = |body: Value| request("PUT", "/api/v1/settings", Some(body));
    let agent = pond.agent.clone();

    let (status, _) = send(&pond, put(json!({"chat_model": "gemma-4-E4B-it-Q4_K_M"}))).await;
    assert_eq!(status, StatusCode::OK);
    eventually("the warm-up after the model change", || {
        agent.prewarms.load(Ordering::SeqCst) == 1
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        pond.agent.prewarms.load(Ordering::SeqCst),
        1,
        "one change, one warm-up"
    );
    assert_eq!(
        pond.agent.prepared(),
        vec!["gemma-4-E4B-it-Q4_K_M".to_string()]
    );

    let (status, _) = send(&pond, put(json!({"chat_model": "gemma-4-E4B-it-Q4_K_M"}))).await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(pond.agent.prewarms.load(Ordering::SeqCst), 1);

    let (status, _) = send(
        &pond,
        put(json!({"chat_provider": "ollama", "chat_model": "gemma4:e2b"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        pond.agent.prepared(),
        vec!["gemma-4-E4B-it-Q4_K_M".to_string()],
        "only a local model is handed to prepare_model"
    );
}

// Speculative decoding was taken out of the llama.cpp engine on 2026-09-24 (goose 743649d98), so
// this is commented out rather than deleted; restore it if it returns.
// // ── PUT /settings: the speculation switch ──────────────────────────────────────
//
// /// The only test in this binary that touches the process-global speculation gate, so the
// /// parallel tests beside it cannot see it flip.
// #[tokio::test]
// async fn the_speculation_switch_applies_now_and_warms_exactly_once_per_change() {
//     use pond_core::models::domain::drafter::speculation_enabled;
//
//     let pond = pond(Arc::new(VisionAgent::default()), "local", E2B).await;
//     let put = |body: Value| request("PUT", "/api/v1/settings", Some(body));
//
//     let (status, body) =
//         send_json(&pond, put(json!({"speculative_decoding_enabled": false}))).await;
//     assert_eq!(status, StatusCode::OK, "{body}");
//     assert!(
//         !speculation_enabled(),
//         "the gate is written by the save itself"
//     );
//     let agent = pond.agent.clone();
//     eventually("the warm-up after turning it off", || {
//         agent.prewarms.load(Ordering::SeqCst) == 1
//     })
//     .await;
//     assert!(
//         !pond
//             .settings
//             .get()
//             .await
//             .unwrap()
//             .speculative_decoding_enabled,
//         "and the choice is stored"
//     );
//
//     // Re-sending the value is not a change: no Warming banner for nothing.
//     let (status, _) = send(&pond, put(json!({"speculative_decoding_enabled": false}))).await;
//     assert_eq!(status, StatusCode::OK);
//     tokio::time::sleep(std::time::Duration::from_millis(100)).await;
//     assert_eq!(pond.agent.prewarms.load(Ordering::SeqCst), 1);
//
//     // A save that changes the model AND the switch starts ONE warm-up, and prepares the model.
//     let (status, _) = send(
//         &pond,
//         put(json!({"speculative_decoding_enabled": true, "chat_model": "gemma-4-E4B-it-Q4_K_M"})),
//     )
//     .await;
//     assert_eq!(status, StatusCode::OK);
//     assert!(speculation_enabled());
//     eventually("the warm-up after the combined change", || {
//         agent.prewarms.load(Ordering::SeqCst) == 2
//     })
//     .await;
//     tokio::time::sleep(std::time::Duration::from_millis(100)).await;
//     assert_eq!(
//         pond.agent.prewarms.load(Ordering::SeqCst),
//         2,
//         "one change, one warm-up"
//     );
//     assert_eq!(
//         pond.agent.prepared(),
//         vec!["gemma-4-E4B-it-Q4_K_M".to_string()]
//     );
//
//     // An Ollama tag that names a Gemma family is not a local GGUF: nothing to provision. Kept in
//     // this test because every PUT writes the gate, and a second PUT test would race this one.
//     let (status, _) = send(
//         &pond,
//         put(json!({"chat_provider": "ollama", "chat_model": "gemma4:e2b"})),
//     )
//     .await;
//     assert_eq!(status, StatusCode::OK);
//     assert_eq!(
//         pond.agent.prepared(),
//         vec!["gemma-4-E4B-it-Q4_K_M".to_string()],
//         "only a local model is handed to prepare_model"
//     );
// }

// ── DELETE: the last model of a family takes its picture support with it ───────

#[tokio::test]
async fn deleting_the_last_model_of_a_family_removes_its_picture_support() {
    let pond = pond(Arc::new(VisionAgent::default()), "local", "Llama-3.2-3B").await;
    let root = pond.tmp.path();
    let gguf = root.join("models").join("gguf");
    std::fs::create_dir_all(&gguf).unwrap();
    let q4 = "gemma-4-E2B-it-Q4_K_M";
    let q5 = "gemma-4-E2B-it-Q5_K_M";
    let qat = "gemma-4-E2B-it-qat-UD-Q4_K_XL";
    for name in [q4, q5, qat] {
        std::fs::write(gguf.join(format!("{name}.gguf")), b"weights").unwrap();
        pond.models
            .upsert(&gguf_row(name, true, None))
            .await
            .unwrap();
    }
    // The Mac's older, mixed-case layout for the plain encoder; the qat one is a separate file.
    let plain = root.join("models/mmproj/gemma-4-E2B-it");
    let qat_dir = root.join("models/mmproj/gemma-4-e2b-it-qat");
    for dir in [&plain, &qat_dir] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("mmproj-BF16.gguf"), b"encoder").unwrap();
    }

    let delete = |name: &str| request("DELETE", &format!("/api/v1/models/gguf/{name}"), None);

    let (status, _) = send(&pond, delete(q4)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(plain.exists(), "Q5_K_M still uses the plain E2B encoder");

    let (status, _) = send(&pond, delete(q5)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!plain.exists(), "no model left uses it, so it goes");
    assert!(
        qat_dir.exists(),
        "the qat encoder is a different file and its model is still here"
    );
}

#[tokio::test]
async fn a_gguf_on_disk_that_no_scan_has_registered_still_keeps_its_picture_support() {
    let pond = pond(Arc::new(VisionAgent::default()), "local", "Llama-3.2-3B").await;
    let root = pond.tmp.path();
    let gguf = root.join("models").join("gguf");
    std::fs::create_dir_all(&gguf).unwrap();
    std::fs::write(gguf.join(format!("{E2B}.gguf")), b"weights").unwrap();
    pond.models
        .upsert(&gguf_row(E2B, true, None))
        .await
        .unwrap();
    // Dropped in by hand: on disk, not in the catalogue.
    std::fs::write(gguf.join("gemma-4-E2B-it-Q8_0.gguf"), b"weights").unwrap();
    let encoder = root.join("models/mmproj/gemma-4-e2b-it");
    std::fs::create_dir_all(&encoder).unwrap();
    std::fs::write(encoder.join("mmproj-BF16.gguf"), b"encoder").unwrap();

    let (status, _) = send(
        &pond,
        request("DELETE", &format!("/api/v1/models/gguf/{E2B}"), None),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(encoder.exists());
}
