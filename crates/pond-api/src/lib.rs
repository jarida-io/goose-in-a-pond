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
//! All clients are rate limited to 100 requests per 60 seconds.

pub mod middleware;
pub mod routes;

use axum::{middleware::Next, Router};
use pond_core::ports::agent::Agent;
use pond_core::ports::handshake::Handshake;
use pond_core::ports::onboarding::OnboardingRepository;
use pond_core::ports::provider::LlmProvider;
use pond_core::ports::session_storage::SessionStorage;
use pond_core::ports::camera_storage::CameraStorage;
use pond_core::ports::device_registry::DeviceRegistry;
use pond_core::ports::embedding::EmbeddingProvider;
use pond_core::ports::memory_repository::MemoryRepository;
use pond_core::ports::profile::ProfileRepository;
use pond_core::ports::sensor_storage::SensorStorage;
use pond_core::ports::settings::SettingsRepository;
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
    /// LLM provider for AI-generated responses. `None` → echo via agent.
    pub llm_provider: Option<Arc<dyn LlmProvider>>,
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
    /// Live model status snapshot — updated by the registry refresh endpoint.
    /// `None` in tests that don't exercise model endpoints.
    pub model_status: Option<Arc<tokio::sync::RwLock<Vec<ModelStatusEntry>>>>,
    /// GIAP data directory — used by model endpoints to check file presence on disk.
    /// `None` in tests.
    pub data_dir: Option<std::path::PathBuf>,
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
}

/// Build the full API router.
///
/// Web dashboard: `/{route_name}`
/// REST API:      `/api/v1/{route_name}`
pub fn build_router(state: Arc<AppState>, static_dir: std::path::PathBuf) -> Router {
    // Create rate limiter: 100 requests per 60 seconds per client
    let rate_limiter = Arc::new(middleware::RateLimiter::new(
        100,
        std::time::Duration::from_secs(60),
    ));

    Router::new()
        // Dev test page — no auth required, returns HTML
        .route("/dev/test", axum::routing::get(routes::dev_test_page))
        .nest("/api/v1", routes::api_routes(state.clone()))
        .fallback_service(routes::web_routes(static_dir))
        // Log every request/response at DEBUG level.
        .layer(axum::middleware::from_fn(middleware::log_requests))
        // Authenticate protected API routes via Bearer token.
        // Public routes (health, handshake, onboard, etc.) bypass this check.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_middleware,
        ))
        // Apply rate limiting to all routes
        .layer(axum::middleware::from_fn(move |req, next| {
            let limiter = rate_limiter.clone();
            rate_limit_with_limiter(req, next, limiter)
        }))
        .with_state(state)
}

async fn rate_limit_with_limiter(
    req: axum::extract::Request,
    next: Next,
    limiter: Arc<middleware::RateLimiter>,
) -> Result<axum::response::Response, middleware::AuthError> {
    // Extract client IP from ConnectInfo if available
    let client_ip = req
        .extensions()
        .get::<std::net::SocketAddr>()
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    if !limiter.check_rate_limit(&client_ip).await {
        return Err(middleware::AuthError::RateLimitExceeded);
    }
    Ok(next.run(req).await)
}
