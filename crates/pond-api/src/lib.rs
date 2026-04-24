//! Pond API — REST routes for Goose In A Pond
//!
//! All routes are versioned under `/api/v1/`.
//!
//! # Route plan
//!
//! ## Onboarding
//! - `POST /api/v1/handshake`  — GIAP ↔ GOTG handshake
//! - `POST /api/v1/onboard`    — Start onboarding flow
//! - `GET  /api/v1/onboard/status` — Check onboarding state
//!
//! ## Chat / Agent
//! - `POST /api/v1/chat`       — Send a message, get a response
//! - `GET  /api/v1/sessions`   — List sessions
//!
//! ## System
//! - `GET  /api/v1/health`     — Health check
//! - `GET  /api/v1/system/info` — System info (hostname, version, etc.)
//!
//! ## Devices
//! - `GET  /api/v1/devices`    — List registered devices
//! - `POST /api/v1/devices`    — Register a new device
//!
//! ## Settings
//! - `GET  /api/v1/settings`   — Get current settings
//! - `PUT  /api/v1/settings`   — Update settings
//!
//! # Authentication
//! Protected routes require a bearer token in the Authorization header:
//! ```text
//! Authorization: Bearer <token>
//! ```
//!
//! Get a token via POST /api/v1/handshake
//!
//! # Rate Limiting
//! All clients are rate limited to 600 requests per 60 seconds (10 req/s burst).

pub mod middleware;
pub mod routes;

/// Controls the lifecycle of the local llamafile server process.
///
/// Injected by `pond-server` so `pond-api` has no process-management logic.
/// `None` in tests — route handlers check `Option<Arc<dyn LlamafileManager>>`.
#[async_trait::async_trait]
pub trait LlamafileManager: Send + Sync {
    /// Ensure the llamafile server is running, starting it if necessary.
    ///
    /// `model_name` hints which model to start; the implementation resolves
    /// it from the model catalog if `None` or empty.
    ///
    /// Returns the base URL (`http://127.0.0.1:<port>`) the server is
    /// reachable at (even if startup is still in progress).
    async fn ensure_started(&self, model_name: Option<&str>) -> String;

    /// Returns `true` if the llamafile server is currently answering requests.
    async fn is_running(&self) -> bool;

    /// Ensure the llamafile server is running AND block until it is ready,
    /// up to `timeout_secs` seconds.
    ///
    /// Returns `(url, true)` when the server becomes ready within the timeout,
    /// or `(url, false)` if it did not become ready in time.
    /// `url` is always the base URL (`http://127.0.0.1:<port>`) regardless of outcome.
    async fn ensure_started_and_wait(
        &self,
        model_name: Option<&str>,
        timeout_secs: u64,
    ) -> (String, bool);
}

use axum::{middleware::Next, Router};
use tower_http::cors::{Any, CorsLayer};
use pond_core::ports::agent::Agent;
use pond_core::ports::handshake::Handshake;
use pond_core::ports::onboarding::OnboardingRepository;
use pond_core::ports::provider::LlmProvider;
use pond_core::ports::session_storage::SessionStorage;
use pond_core::ports::camera_storage::CameraStorage;
use pond_core::ports::device_registry::DeviceRegistry;
use pond_core::ports::embedding::EmbeddingProvider;
use pond_core::ports::mcp_memory::McpMemoryPort;
use pond_core::ports::model_catalog_provider::ModelCatalogProvider;
use pond_core::ports::model_repository::ModelRepository;
use pond_core::ports::model_scheduler::ModelScheduler;
use pond_core::ports::memory_repository::MemoryRepository;
use pond_core::ports::extension_manager::ExtensionManagerPort;
use pond_core::ports::mcp_server::McpServerRepository;
use pond_core::ports::profile::ProfileRepository;
use pond_core::ports::prompt_extra::PromptExtraRepository;
use pond_core::ports::prompt_template::PromptTemplateRepository;
use pond_core::ports::recipe::AgentRecipeRepository;
use pond_core::ports::scheduler::SchedulerPort;
use pond_core::ports::sensor_storage::SensorStorage;
use pond_core::ports::settings::SettingsRepository;
use pond_core::ports::skill::UserSkillRepository;
use pond_core::ports::voice_output::VoiceOutput;
use pond_infra::db::Database;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Shared application state available to all route handlers.
pub struct AppState {
    pub db: Arc<Database>,
    pub handshake: Arc<dyn Handshake>,
    pub onboarding_repo: Arc<dyn OnboardingRepository + Send + Sync>,
    /// Base URL of the whisper.cpp server (e.g. "http://127.0.0.1:9000").
    pub whisper_url: String,
    /// Session storage for conversation persistence.
    pub session_storage: Arc<dyn SessionStorage>,
    /// Shared HTTP client — reuse across requests to get connection pooling.
    pub http_client: reqwest::Client,
    /// Agent used as fallback when no LLM provider is configured.
    pub agent: Arc<dyn Agent>,
    /// LLM provider for AI-generated responses, wrapped in a RwLock so the
    /// ModelRouter can be hot-swapped when the user changes role assignments.
    /// `None` inside the lock → echo via agent.
    pub llm_provider: Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>,
    /// Base URL of the local llamafile server — stored here so the settings
    /// handler can rebuild the ModelRouter without restarting the server.
    pub llamafile_url: String,
    /// TTS engine for the `/api/v1/test/speak` dev endpoint. `None` → print only.
    pub tts: Option<Arc<dyn VoiceOutput>>,
    /// Persistent settings repository (assistant identity, LLM, voice, retention).
    pub settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    /// Profile repository for household members.
    pub profile_repo: Arc<dyn ProfileRepository + Send + Sync>,
    /// Device registry for GOTG devices and other connected hardware.
    pub device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    /// Memory fragment repository for semantic/recency-based retrieval.
    pub memory_repo: Arc<dyn MemoryRepository + Send + Sync>,
    /// Embedding provider — `None` until a real embedding model is configured.
    pub embedding_provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    /// IoT sensor reading storage (uses logs DB).
    pub sensor_storage: Arc<dyn SensorStorage + Send + Sync>,
    /// Camera event storage (uses logs DB).
    pub camera_storage: Arc<dyn CameraStorage + Send + Sync>,
    /// Directory to look for user-supplied prompt overrides (e.g. `system.md`).
    /// Mirrors Goose's `~/.config/goose/prompts/` pattern.
    /// `None` in tests; `Some($DATA_DIR/prompts)` in production.
    pub prompt_template_dir: Option<std::path::PathBuf>,
    /// Persistent model catalog — replaces the old in-memory snapshot.
    /// `None` in tests that don't exercise model endpoints.
    pub model_repo: Option<Arc<dyn ModelRepository + Send + Sync>>,
    /// GIAP data directory — used by model endpoints to check file presence on disk.
    /// `None` in tests.
    pub data_dir: Option<std::path::PathBuf>,
    /// Skip onboarding check in middleware. Set to `true` in tests.
    pub skip_onboarding: bool,
    /// Cron-based task scheduler. `None` until `pond-infra-scheduler` is wired in.
    pub scheduler: Option<Arc<dyn SchedulerPort>>,
    /// Memory-aware model scheduler. `None` when all roles use external providers.
    pub model_scheduler: Option<Arc<dyn ModelScheduler>>,
    /// MCP-style persistent memory. `None` until `pond-adapters-mcp-memory` is wired in.
    pub mcp_memory: Option<Arc<dyn McpMemoryPort + Send + Sync>>,
    /// MCP extension manager — manages Goose extensions for tool calling.
    /// `None` until a Goose agent with extension support is wired in.
    pub extension_manager: Option<Arc<dyn ExtensionManagerPort>>,
    /// Persistent storage for configured external MCP server connections.
    /// Loaded at startup to auto-connect saved servers.
    pub mcp_server_repo: Option<Arc<dyn McpServerRepository>>,

    /// Tracks in-progress model downloads so the UI can show progress bars.
    pub download_tracker: Arc<tokio::sync::RwLock<std::collections::HashMap<String, DownloadEntry>>>,
    /// Port the Piper HTTP TTS server is listening on.
    /// Set at startup by `piper_http::start()`. `None` if Piper is not running.
    pub piper_http_port: Option<u16>,
    /// Catalog provider — fetches the online model registry and returns typed records.
    /// Injected by pond-server so pond-api has no HTTP or parsing logic.
    /// `None` in tests.
    pub model_catalog_provider: Option<Arc<dyn ModelCatalogProvider>>,
    /// Filesystem storage helper — resolves on-disk paths for model records.
    /// Used by the refresh handler to update `downloaded` flags.
    /// `None` in tests.
    pub model_storage_dir: Option<std::path::PathBuf>,
    /// System prompt templates — editable by user, seeded from defaults at setup.
    /// `None` in tests that don't exercise prompt template endpoints.
    pub prompt_template_repo: Option<Arc<dyn PromptTemplateRepository + Send + Sync>>,
    /// Per-key extra instructions injected into the system prompt each turn.
    /// `None` in tests that don't exercise prompt extra endpoints.
    pub prompt_extra_repo: Option<Arc<dyn PromptExtraRepository + Send + Sync>>,
    /// User-defined skills injected as named system prompt extras.
    /// `None` in tests that don't exercise skill endpoints.
    pub skill_repo: Option<Arc<dyn UserSkillRepository + Send + Sync>>,
    /// Agent recipes (Goose Recipe YAML definitions).
    /// `None` in tests that don't exercise recipe endpoints.
    pub recipe_repo: Option<Arc<dyn AgentRecipeRepository + Send + Sync>>,
    /// Controls the llamafile server lifecycle.
    /// Set by pond-server when `chat_provider` may be "llamafile".
    /// `None` in tests and when the llamafile backend is not available.
    pub llamafile_manager: Option<Arc<dyn LlamafileManager>>,
    /// Read/write access to the `event_log` table in `pond_logs.db`.
    /// Used by the `/api/v1/logs` endpoint.
    /// `None` in tests.
    pub event_log_repo: Option<Arc<dyn pond_core::ports::event_log::EventLogRepository>>,
    /// Speaker identification adapter. `None` when no ONNX model is configured.
    pub speaker_id: Option<Arc<dyn pond_core::ports::speaker_id::SpeakerIdentification + Send + Sync>>,
}

/// State of a single in-progress (or recently completed) model download.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DownloadEntry {
    pub filename:         String,
    pub category:         String,
    pub downloaded_bytes: u64,
    pub total_bytes:      Option<u64>,
    /// "downloading" | "done" | "error"
    pub status:           String,
}

/// Snapshot of one model's availability, sent over the REST API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatusEntry {
    pub category:    String,
    pub name:        String,
    pub description: String,
    pub size_mb:     u64,
    /// True if the model file exists on disk (or for HTTP TTS, always true).
    pub downloaded:  bool,
    /// True if this is the currently active model for its category.
    pub active:      bool,
    /// Download URL — None for HTTP TTS entries that have no downloadable file.
    pub url:      Option<String>,
    /// HuggingFace model spec (GGUF only): "author/repo:quantization"
    pub hf_id:    Option<String>,
    /// Filename on disk (used by the download route to determine the save path)
    pub filename: Option<String>,
    /// Approximate RAM required at runtime in MB. None for models without estimates.
    pub ram_estimate_mb: Option<u64>,
    /// Suggested role assignment: "chat" | "think" | "task". None = general purpose.
    pub recommended_role: Option<String>,
}

/// Build the full API router.
///
/// Web dashboard: `/{route_name}`
/// REST API:      `/api/v1/{route_name}`
pub fn build_router(state: Arc<AppState>, static_dir: std::path::PathBuf) -> Router {
    // Rate limiter for remote clients (GOTG app, external integrations).
    // 600 req/60s = 10 req/s burst — generous for API use, still protects against abuse.
    // Loopback clients (local web dashboard) are exempted entirely in the middleware.
    let rate_limiter = Arc::new(middleware::RateLimiter::new(
        600,
        std::time::Duration::from_secs(60),
    ));

    Router::new()
        // Dev test page — no auth required, returns HTML
        .route("/dev/test", axum::routing::get(routes::dev_test_page))
        .nest("/api/v1", routes::api_routes(state.clone()))
        .fallback_service(routes::web_routes(static_dir))
        // Log every request/response at DEBUG level.
        .layer(axum::middleware::from_fn(middleware::log_requests))
        // Enforce Bearer token authentication on all protected routes.
        // Must come AFTER log_requests (layers apply in reverse order in axum).
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_middleware,
        ))
        // Apply rate limiting to all routes
        .layer(axum::middleware::from_fn(move |req, next| {
            let limiter = rate_limiter.clone();
            rate_limit_with_limiter(req, next, limiter)
        }))
        // CORS — allow any origin so the Tauri desktop app (tauri://localhost or
        // http://localhost:1420 in dev) and GOTG mobile clients can reach the API.
        // pond-server only binds to the local network, so open CORS is safe here.
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state)
}

async fn rate_limit_with_limiter(
    req: axum::extract::Request,
    next: Next,
    limiter: Arc<middleware::RateLimiter>,
) -> Result<axum::response::Response, middleware::AuthError> {
    // Extract client IP from ConnectInfo<SocketAddr> (populated by
    // into_make_service_with_connect_info in main.rs).
    let connect_info = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0);

    let client_ip = connect_info
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Loopback clients are the local web dashboard — never rate limit them.
    // Rate limiting only applies to remote clients (GOTG app, external integrations).
    if connect_info.map(|a| a.ip().is_loopback()).unwrap_or(false) {
        return Ok(next.run(req).await);
    }

    if !limiter.check_rate_limit(&client_ip).await {
        return Err(middleware::AuthError::RateLimitExceeded);
    }
    Ok(next.run(req).await)
}
