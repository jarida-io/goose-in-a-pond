//! REST API under `/api/v1/`; protected routes take a Bearer token from `POST /api/v1/handshake`.

pub mod cleanup;
pub mod middleware;
pub mod oauth_callback;
pub mod routes;
pub mod runs;
pub mod thought_filter;
pub mod tool_context;

/// Local llamafile server lifecycle, injected by `pond-server` (`None` in tests).
#[async_trait::async_trait]
pub trait LlamafileManager: Send + Sync {
    /// Start the server if needed and return its base URL, even while startup is still running.
    /// An empty or `None` `model_name` is resolved from the model catalog.
    async fn ensure_started(&self, model_name: Option<&str>) -> String;

    /// Returns `true` if the llamafile server is currently answering requests.
    async fn is_running(&self) -> bool;

    /// As `ensure_started` but waits up to `timeout_secs`; returns `(url, ready_before_timeout)`.
    async fn ensure_started_and_wait(
        &self,
        model_name: Option<&str>,
        timeout_secs: u64,
    ) -> (String, bool);
}

use axum::{middleware::Next, Router};
use pond_adapters_weather::WeatherProvider;
use pond_core::context::vector_index::VectorIndex;
use pond_core::mcp::ports::extension_manager::ExtensionManagerPort;
use pond_core::mcp::ports::extension_marketplace::ExtensionMarketplace;
use pond_core::mcp::ports::mcp_knowledge::McpKnowledgePort;
use pond_core::mcp::ports::mcp_server::McpServerRepository;
use pond_core::models::ports::agent::Agent;
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::models::ports::model_catalog_provider::ModelCatalogProvider;
use pond_core::models::ports::model_repository::ModelRepository;
use pond_core::models::ports::model_scheduler::ModelScheduler;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::security::ports::handshake::Handshake;
use pond_core::security::ports::policy::SecurityPolicy;
use pond_core::security::ports::telemetry::TelemetryPort;
use pond_core::shared::ports::event_bus::EventBus;
use pond_core::user_data::ports::camera_storage::CameraStorage;
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::face_recognition::FaceRecognition;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::prompt_extra::PromptExtraRepository;
use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;
use pond_core::user_data::ports::recipe::AgentRecipeRepository;
use pond_core::user_data::ports::scheduler::SchedulerPort;
use pond_core::user_data::ports::sensor_storage::SensorStorage;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::skill::UserSkillRepository;
use pond_infra::db::Database;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::CorsLayer;

/// Spawns the memory consolidation pipeline (injected by `pond-server`); the token aborts it.
pub type ConsolidationRunner = Arc<
    dyn Fn(
            tokio_util::sync::CancellationToken,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Shared application state available to all route handlers.
pub struct AppState {
    pub db: Arc<Database>,
    pub handshake: Arc<dyn Handshake>,
    pub onboarding_repo: Arc<dyn OnboardingRepository + Send + Sync>,
    /// Base URL of the whisper.cpp server (e.g. "http://127.0.0.1:9000").
    pub whisper_url: String,
    /// In-process WAV → transcript; if set, `/api/v1/transcribe` uses it, not `whisper_url`.
    pub transcribe_audio: Option<Arc<dyn Fn(Vec<u8>) -> anyhow::Result<String> + Send + Sync>>,
    pub session_storage: Arc<dyn SessionStorage>,
    /// Shared HTTP client — reuse across requests to get connection pooling.
    pub http_client: reqwest::Client,
    /// Agent used as fallback when no LLM provider is configured.
    pub agent: Arc<dyn Agent>,
    /// Warm-up status for `GET /api/v1/warmup`; `std` lock since its writer is a sync callback.
    pub warmup: Arc<std::sync::RwLock<WarmupStatus>>,
    /// Hot-swappable when role assignments change; `None` inside the lock falls back to `agent`.
    pub llm_provider: Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>,
    /// Local llamafile base URL, kept so settings changes can rebuild the ModelRouter live.
    pub llamafile_url: String,
    /// TTS engine for the `/api/v1/test/speak` dev endpoint. `None` → print only.
    pub tts: Option<Arc<dyn VoiceOutput>>,
    /// Live speech-engine reconfiguration (voice, pace, quality); `None` when no engine runs.
    pub tts_control: Option<Arc<dyn pond_core::models::ports::tts_control::TtsControl>>,
    /// Persistent settings repository (assistant identity, LLM, voice, retention).
    pub settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    /// Profile repository for household members.
    pub profile_repo: Arc<dyn ProfileRepository + Send + Sync>,
    /// Device registry for GOTG devices and other connected hardware.
    pub device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    /// Matter runtime, reconciled from `matter_enabled`; `None` only where no Matter is wired.
    /// Off is a runtime state, so an unreachable controller never reads as "not enabled".
    pub matter: Option<Arc<dyn pond_core::user_data::ports::matter_runtime::MatterRuntimePort>>,
    /// Memory fragment repository for semantic/recency-based retrieval.
    pub memory_repo: Arc<dyn MemoryRepository + Send + Sync>,
    /// `None` until a real embedding model is configured.
    pub embedding_provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    /// The personal-context index retrieval reads; `None` on CLI paths and in tests.
    /// Don't copy the model id here: [`Self::embedding_provider`] owns it, and a copy drifts.
    pub vector_index: Option<Arc<dyn VectorIndex>>,
    /// Wakes the maintenance sweep to refill after the rebuild route clears the index.
    /// `None` with no sweep to wake; the route still clears, and the next process refills.
    pub index_reindex: Option<Arc<tokio::sync::Notify>>,
    /// Sync all connected accounts now rather than on the timer; `None` where nothing can sync.
    pub account_sync: Option<Arc<dyn pond_core::context::ports::AccountSync>>,
    /// IoT sensor reading storage (uses logs DB).
    pub sensor_storage: Arc<dyn SensorStorage + Send + Sync>,
    /// Camera event storage (uses logs DB).
    pub camera_storage: Arc<dyn CameraStorage + Send + Sync>,
    /// `$DATA_DIR/prompts`: user prompt overrides (e.g. `system.md`); `None` in tests.
    pub prompt_template_dir: Option<std::path::PathBuf>,
    /// Persistent model catalog; `None` in tests that don't exercise model endpoints.
    pub model_repo: Option<Arc<dyn ModelRepository + Send + Sync>>,
    /// GIAP data directory, used to check model files on disk; `None` in tests.
    pub data_dir: Option<std::path::PathBuf>,
    /// Skip onboarding check in middleware. Set to `true` in tests.
    pub skip_onboarding: bool,
    /// Cron-based task scheduler. `None` until `pond-infra-scheduler` is wired in.
    pub scheduler: Option<Arc<dyn SchedulerPort>>,
    /// Memory-aware model scheduler. `None` when all roles use external providers.
    pub model_scheduler: Option<Arc<dyn ModelScheduler>>,
    /// MCP-style persistent memory. `None` until `pond-adapters-mcp-memory` is wired in.
    pub mcp_memory: Option<Arc<dyn McpKnowledgePort + Send + Sync>>,
    /// Goose extension manager; `None` until a Goose agent with extensions is wired in.
    pub extension_manager: Option<Arc<dyn ExtensionManagerPort>>,
    /// Saved external MCP servers, auto-connected at startup.
    pub mcp_server_repo: Option<Arc<dyn McpServerRepository>>,
    /// All built-in + extension tools; routes resync it as extensions come and go.
    pub tool_registry:
        Option<Arc<dyn pond_core::mcp::ports::tools::tool_registry::ToolRegistryPort>>,
    /// Runs a tool by name without the LLM, for `POST /api/v1/tools/invoke`.
    pub tool_dispatcher:
        Option<Arc<dyn pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher>>,
    /// Extension marketplace — curated registry of installable MCP extensions.
    pub marketplace: Option<Arc<dyn ExtensionMarketplace>>,
    /// Secure secret storage for extension API keys and OAuth tokens.
    pub secret_repo:
        Option<Arc<dyn pond_core::security::ports::secret::SecretRepository + Send + Sync>>,

    /// Tracks in-progress model downloads so the UI can show progress bars.
    pub download_tracker:
        Arc<tokio::sync::RwLock<std::collections::HashMap<String, DownloadEntry>>>,
    /// Piper HTTP TTS port, set by `piper_http::start()`; `None` if Piper isn't running.
    pub piper_http_port: Option<u16>,
    /// Fetches the online model registry; injected by pond-server, `None` in tests.
    pub model_catalog_provider: Option<Arc<dyn ModelCatalogProvider>>,
    /// Where model files live; the refresh handler checks it for `downloaded` flags.
    pub model_storage_dir: Option<std::path::PathBuf>,
    /// User-editable system prompt templates, seeded from defaults at setup.
    pub prompt_template_repo: Option<Arc<dyn PromptTemplateRepository + Send + Sync>>,
    /// Per-key extra instructions injected into the system prompt each turn.
    pub prompt_extra_repo: Option<Arc<dyn PromptExtraRepository + Send + Sync>>,
    /// User-defined skills injected as named system prompt extras.
    pub skill_repo: Option<Arc<dyn UserSkillRepository + Send + Sync>>,
    /// Agent recipes (Goose Recipe YAML definitions).
    pub recipe_repo: Option<Arc<dyn AgentRecipeRepository + Send + Sync>>,
    /// Set by pond-server when `chat_provider` may be "llamafile"; otherwise `None`.
    pub llamafile_manager: Option<Arc<dyn LlamafileManager>>,
    /// The `event_log` table in `pond_logs.db`, behind `/api/v1/logs`.
    pub operational_log:
        Option<Arc<dyn pond_core::security::ports::event_log::OperationalLogRepository>>,
    /// In-process event bus that sensor and camera records publish to, for reactive consumers.
    pub event_bus: Option<Arc<dyn EventBus>>,
    /// Append-only `events` table fed by the bus→log bridge; read by the activity API.
    pub event_log: Option<Arc<dyn pond_core::security::ports::event_log::EventLog>>,
    /// Paired devices' current FCM/APNs/Expo tokens; `None` makes the push-token routes 503.
    pub push_token_repo:
        Option<Arc<dyn pond_core::user_data::ports::push_token::PushTokenRepository>>,
    /// Face register/identify; `None` without an ONNX embedding model (face routes then 503).
    pub face_recognition: Option<Arc<dyn FaceRecognition>>,
    /// Caps concurrent chat SSE streams so stalled clients can't grow memory unboundedly.
    pub sse_semaphore: Arc<tokio::sync::Semaphore>,
    /// Detached agent runs: a turn that outlives its connection is owned here (see `crate::runs`).
    pub runs: Arc<crate::runs::RunSupervisor>,
    /// Caps `/notifications/stream`; apart from `sse_semaphore` since phones hold it open.
    pub notification_sse_semaphore: Arc<tokio::sync::Semaphore>,
    /// Post-inference review that triggers a revision when answer quality falls below threshold.
    pub answer_reviewer: Option<Arc<dyn pond_core::models::ports::answer_reviewer::AnswerReviewer>>,
    /// Extracts durable facts from turns; `None` when `memory_extraction_enabled` is false.
    pub memory_extractor:
        Option<Arc<dyn pond_core::user_data::ports::memory_extractor::MemoryExtractor>>,
    /// Shared extraction service instance (rate limiter + dedup state).
    pub memory_extraction_service:
        Option<Arc<pond_core::user_data::services::memory_extraction::MemoryExtractionService>>,
    /// Last user request time, for the inactivity-based consolidation scheduler.
    pub last_user_activity: Arc<tokio::sync::RwLock<std::time::Instant>>,
    /// Cancelled by any user request, stopping in-progress consolidation at once.
    pub consolidation_cancel: Arc<tokio::sync::RwLock<Option<tokio_util::sync::CancellationToken>>>,
    /// Broadcast channel for consolidation events (streamed to SSE for the UI modal).
    pub consolidation_event_tx: tokio::sync::broadcast::Sender<
        pond_core::user_data::ports::memory_consolidator::ConsolidationEvent,
    >,
    /// `None` when `memory_consolidation_enabled` is false.
    pub consolidation_runner: Option<ConsolidationRunner>,
    /// Concurrent LLM tasks, limited per provider (3 for HTTP, 1 for GGUF).
    pub inference_pool: Option<Arc<dyn pond_core::models::ports::inference_pool::InferencePool>>,
    /// Broadcast channel for schedule completion events (SSE + desktop notifications).
    pub schedule_result_tx:
        tokio::sync::broadcast::Sender<pond_core::user_data::domain::schedule::ScheduleResultEvent>,
    /// Fan-out to `/notifications/stream` clients; producers go through `notification_sender`.
    pub notification_tx:
        tokio::sync::broadcast::Sender<pond_core::mcp::ports::notification::Notification>,
    /// Offline queue, flushed on stream connect to a device that was disconnected.
    pub notification_queue:
        Option<Arc<dyn pond_core::mcp::ports::notification_queue::NotificationQueueRepository>>,
    /// What producers (`send_notification`, the schedule bridge) send through.
    pub notification_sender:
        Option<Arc<dyn pond_core::mcp::ports::notification::NotificationSender>>,
    /// Per-turn telemetry recorder. `None` when `telemetry_enabled` is false.
    pub telemetry: Option<Arc<dyn TelemetryPort>>,
    /// Per-session context fill tracking; warns before the "context cliff" where quality drops.
    pub context_monitor: Arc<pond_core::models::services::context_monitor::ContextMonitor>,
    /// `ui://` URI → embedded MCP App HTML, served by `GET /api/v1/mcp/resources`.
    pub mcp_app_resources: std::collections::HashMap<String, &'static str>,
    /// In-memory OAuth PKCE sessions (state nonce -> verifier + provider).
    pub oauth_state: crate::oauth_callback::OAuthState,
    /// Finished OAuth flow outcomes, so the UI needn't infer success from a stored token.
    pub oauth_outcomes: crate::oauth_callback::OAuthOutcomes,
    /// Authz/audit hook, not a gate: the default allows all and only audits; routes opt in.
    pub security_policy: Option<Arc<dyn SecurityPolicy>>,
    /// The port actually bound (not always 4000), for building OAuth redirect URIs.
    pub api_port: u16,
    /// For `GET /api/v1/weather`; `None` if `weather_enabled` is off or no location is set.
    pub weather_provider: Option<Arc<dyn WeatherProvider>>,
    /// Mesh: trusted peers and scopes. Plain SQLite, so always built, unlike `mesh_transport`.
    pub peer_directory:
        Arc<dyn pond_core::mesh::ports::peer_directory::PeerDirectory + Send + Sync>,
    /// Mesh: prepaid-credit balance held with each trusted peer.
    pub credit_ledger: Arc<dyn pond_core::mesh::ports::credit_ledger::CreditLedger + Send + Sync>,
    /// Mesh: metered token usage pending settlement per peer.
    pub usage_tally: Arc<dyn pond_core::mesh::ports::usage_tally::UsageTally + Send + Sync>,
    /// Mesh libp2p transport; `None` unless the `mesh` feature is built and `mesh_enabled` is on.
    /// The `RwLock` lets `mesh_rebuild` hot-enable it from `PUT /api/v1/settings`.
    pub mesh_transport: Arc<
        tokio::sync::RwLock<Option<Arc<dyn pond_core::mesh::ports::mesh_transport::MeshTransport>>>,
    >,
    /// Mesh `LlmProvider` for completions on a trusted peer; `None` unless `mesh_transport` is set.
    /// Build once per enable: a second `MeshInferenceService` would also drain its `recv()` queue.
    pub mesh_provider:
        Arc<tokio::sync::RwLock<Option<Arc<dyn pond_core::models::ports::provider::LlmProvider>>>>,
    /// Mesh: live queries of what a trusted peer offers right now.
    /// Gated like `mesh_provider`: both are handles into one `MeshInferenceService`.
    pub peer_capability_query: Arc<
        tokio::sync::RwLock<
            Option<Arc<dyn pond_core::mesh::ports::peer_capability_query::PeerCapabilityQuery>>,
        >,
    >,
    /// Builds the mesh stack when `mesh_enabled` flips on; `None` without the `mesh` feature.
    /// Always safe to call: no-op if already built or disabled; never tears a built stack down.
    pub mesh_rebuild:
        Option<Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>>,
}

impl AppState {
    /// Record user activity; every route starting real user work must call this FIRST.
    /// It cancels consolidation, which would otherwise hold the Jetson's one inference slot.
    pub async fn note_user_activity(&self) {
        *self.last_user_activity.write().await = std::time::Instant::now();
        if let Some(cancel) = self.consolidation_cancel.read().await.as_ref() {
            cancel.cancel();
        }
    }
}

/// Prefix warm-up status (`Agent::prewarm`); process-wide, as the prefix serves every session.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WarmupStatus {
    #[serde(flatten)]
    pub phase: pond_core::models::ports::agent::WarmupPhase,
    /// The chat model the warm-up ran (or is running) against.
    pub model: String,
    pub started_unix_ms: u64,
    pub finished_unix_ms: Option<u64>,
}

impl Default for WarmupStatus {
    fn default() -> Self {
        Self {
            phase: pond_core::models::ports::agent::WarmupPhase::Skipped {
                reason: "not yet run".to_string(),
            },
            model: String::new(),
            started_unix_ms: 0,
            finished_unix_ms: None,
        }
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Background prefix warm-up, mirrored into `state.warmup`; a failed warm-up just starts cold.
pub fn spawn_prefix_prewarm(state: Arc<AppState>, voice_mode: bool) {
    tokio::spawn(async move {
        let model = state
            .settings_repo
            .get()
            .await
            .map(|s| s.chat_model)
            .unwrap_or_default();
        {
            let mut w = state.warmup.write().unwrap_or_else(|e| e.into_inner());
            *w = WarmupStatus {
                phase: pond_core::models::ports::agent::WarmupPhase::Warming,
                model,
                started_unix_ms: unix_ms(),
                finished_unix_ms: None,
            };
        }
        let warm = state.warmup.clone();
        let progress = Arc::new(move |phase: pond_core::models::ports::agent::WarmupPhase| {
            let mut w = warm.write().unwrap_or_else(|e| e.into_inner());
            use pond_core::models::ports::agent::WarmupPhase as P;
            if !matches!(phase, P::Warming) {
                w.finished_unix_ms = Some(unix_ms());
            }
            w.phase = phase;
        });
        state.agent.prewarm(voice_mode, progress).await;
    });
}

/// Keep going.
pub const DL_RUN: u8 = 0;
/// Stop, but leave the partial file so it can be picked up again.
pub const DL_PAUSE: u8 = 1;
/// Stop and throw the partial file away.
pub const DL_CANCEL: u8 = 2;

/// State of a single in-progress (or recently completed) model download.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DownloadEntry {
    pub filename: String,
    pub category: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    /// "downloading" | "paused" | "done" | "error" | "cancelled"
    pub status: String,
    /// When it reached "done" or "error"; used to evict stale entries.
    #[serde(skip)]
    pub finished_at: Option<std::time::Instant>,
    /// What this download has been told to do: [`DL_RUN`], [`DL_PAUSE`] or [`DL_CANCEL`].
    /// Read between chunks, the only point a streaming download can stop.
    #[serde(skip)]
    pub control: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Source URL; re-requesting it resumes a paused download from its `.incomplete` file.
    #[serde(skip)]
    pub url: Option<String>,
}

/// Snapshot of one model's availability, sent over the REST API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatusEntry {
    pub category: String,
    pub name: String,
    pub description: String,
    pub size_mb: u64,
    /// True if the model file exists on disk (or for HTTP TTS, always true).
    pub downloaded: bool,
    /// True if this is the currently active model for its category.
    pub active: bool,
    /// Download URL — None for HTTP TTS entries that have no downloadable file.
    pub url: Option<String>,
    /// HuggingFace model spec (GGUF only): "author/repo:quantization"
    pub hf_id: Option<String>,
    /// Filename on disk (used by the download route to determine the save path)
    pub filename: Option<String>,
    /// Approximate RAM required at runtime in MB. None for models without estimates.
    pub ram_estimate_mb: Option<u64>,
    /// Suggested role assignment: "chat" | "think" | "task". None = general purpose.
    pub recommended_role: Option<String>,
    /// Declared max context tokens (LLM rows only); `None` if not LLM or the catalog can't say.
    pub context_length: Option<u32>,
    /// ASR language ("en", "multilingual"). Whisper models only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asr_language: Option<String>,
    /// ASR model size ("tiny", "base", "small", …). Whisper models only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asr_size: Option<String>,
    /// TTS engine identifier ("piper"). TTS models only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_engine: Option<String>,
    /// Companion config filename (.onnx.json). TTS models only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_filename: Option<String>,
}

/// True when the web UI is embedded in this binary (`pond-desktop/dist` existed at build).
pub fn web_ui_embedded() -> bool {
    routes::embedded_ui_present()
}

/// Build the full API router: web dashboard at `/{route}`, REST API under `/api/v1/`.
pub fn build_router(state: Arc<AppState>, static_dir: std::path::PathBuf) -> Router {
    // Remote clients only: loopback (the local dashboard) is exempt in the middleware.
    let rate_limiter = Arc::new(middleware::RateLimiter::new(
        600,
        std::time::Duration::from_secs(60),
    ));
    // Pairing has its own small budget so a chatty client can't lock a device out of `/handshake`.
    // `/handshake/verify` adds a stricter one (`routes::verify_limiter`).
    let handshake_limiter = Arc::new(middleware::RateLimiter::new(
        30,
        std::time::Duration::from_secs(60),
    ));

    Router::new()
        // Dev test page — no auth required, returns HTML
        .route("/dev/test", axum::routing::get(routes::dev_test_page))
        .route("/dev/face", axum::routing::get(routes::dev_face_page))
        .nest("/api/v1", routes::api_routes(state.clone()))
        // Web UI: embedded when built in, else from `static_dir` (dev).
        .fallback(move |uri: axum::http::Uri| routes::serve_web(uri, static_dir.clone()))
        .layer(axum::middleware::from_fn(middleware::log_requests))
        // Must come after log_requests: axum applies layers in reverse order.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_middleware,
        ))
        .layer(axum::middleware::from_fn(move |req, next| {
            let limiter = rate_limiter.clone();
            let handshake_limiter = handshake_limiter.clone();
            rate_limit_with_limiter(req, next, limiter, handshake_limiter)
        }))
        // CORS allows only first-party desktop origins (plus `POND_CORS_ALLOWED_ORIGINS`).
        // Native GOTG clients send no `Origin`, so they are unaffected.
        .layer(build_cors_layer())
        .with_state(state)
}

/// Build the CORS layer with a scoped origin allowlist (see call site).
fn build_cors_layer() -> CorsLayer {
    use axum::http::{header, HeaderValue, Method};

    let mut origins: Vec<HeaderValue> = [
        // Packaged desktop renderer: a custom scheme, since file:// sends `Origin: null`.
        "app://giap",
        // Vite dev server, loaded by the shell in development.
        "http://localhost:1420",
        "http://127.0.0.1:1420",
    ]
    .iter()
    .filter_map(|o| o.parse().ok())
    .collect();
    if let Ok(extra) = std::env::var("POND_CORS_ALLOWED_ORIGINS") {
        for o in extra.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            if let Ok(v) = o.parse() {
                origins.push(v);
            }
        }
    }

    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

/// True for pairing endpoints (own rate budget); segment-matched so `/handshakes` is not one.
fn is_handshake_path(path: &str) -> bool {
    match path.strip_prefix("/api/v1/handshake") {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

async fn rate_limit_with_limiter(
    req: axum::extract::Request,
    next: Next,
    limiter: Arc<middleware::RateLimiter>,
    handshake_limiter: Arc<middleware::RateLimiter>,
) -> Result<axum::response::Response, middleware::AuthError> {
    // Needs `into_make_service_with_connect_info` in main.rs to populate `ConnectInfo`.
    let connect_info = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0);

    let client_ip = connect_info
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Loopback is the local web dashboard: never rate limited.
    if connect_info.map(|a| a.ip().is_loopback()).unwrap_or(false) {
        return Ok(next.run(req).await);
    }

    let limiter = if is_handshake_path(req.uri().path()) {
        &handshake_limiter
    } else {
        &limiter
    };
    if let Err(remaining) = limiter.check_rate_limit_detailed(&client_ip).await {
        return Err(middleware::AuthError::RateLimitExceeded {
            retry_after_secs: middleware::retry_after_secs(remaining),
        });
    }
    Ok(next.run(req).await)
}

#[cfg(test)]
mod rate_limit_tests {
    use super::*;

    #[test]
    fn every_handshake_route_uses_the_pairing_budget() {
        for path in [
            "/api/v1/handshake",
            "/api/v1/handshake/init",
            "/api/v1/handshake/verify",
            "/api/v1/handshake/refresh",
            "/api/v1/handshake/revoke",
            "/api/v1/handshake/pairing-code",
        ] {
            assert!(is_handshake_path(path), "{path} should use it");
        }
    }

    #[test]
    fn ordinary_api_routes_do_not_use_the_pairing_budget() {
        for path in [
            "/api/v1/devices/abc/push-token",
            "/api/v1/chat/stream",
            "/api/v1/notifications/stream",
            "/api/v1/activity",
            "/api/v1/settings",
            // Near-misses: neither is a pairing route.
            "/api/v1/handshakes",
            "/api/v1/device-handshake",
        ] {
            assert!(!is_handshake_path(path), "{path} should not use it");
        }
    }

    #[tokio::test]
    async fn exhausting_the_api_budget_leaves_pairing_available() {
        let api = middleware::RateLimiter::new(2, std::time::Duration::from_secs(60));
        let handshake = middleware::RateLimiter::new(2, std::time::Duration::from_secs(60));

        let ip = "192.0.2.10";
        while api.check_rate_limit(ip).await {}
        assert!(!api.check_rate_limit(ip).await, "API budget is spent");

        assert!(
            handshake.check_rate_limit(ip).await,
            "the same IP can still pair"
        );
    }
}
