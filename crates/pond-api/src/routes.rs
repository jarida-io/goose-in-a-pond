//! Route definitions for GIAP REST API and web dashboard.
//!
//! # TODO
//! - [ ] Implement each handler with real logic
//! - [ ] Add request/response types in pond-core domain
//! - [ ] Serve static web dashboard files

use axum::{
    body::Body,
    extract::{rejection::JsonRejection, Multipart, Path, Query, State},
    http::{Response, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Json,
    },
    routing::{delete, get, patch, post, put},
    Router,
};
use pond_core::mcp::ports::extension_manager::ExtensionInfo;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::prompts::{
    build_system_prompt_with_profile, builtin_template_content, render_template, sanitize_field,
    ProfileContext,
};
use pond_core::security::domain::event::{EventCategory, EventQuery, PrivacySensitivity};
use pond_core::security::ports::handshake::{
    ChallengeResponse, HandshakeRequest, HandshakeResponse, InitRequest, RefreshRequest,
    VerifyRequest,
};
use pond_core::shared::ports::event_bus::BusEvent;
use pond_core::shared::services::chat::ChatService;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::domain::schedule::TaskKind;
use pond_core::user_data::domain::sensor::{CameraEvent, SensorReading};
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_core::user_data::ports::scheduler::{CreateScheduleRequest, UpdateScheduleRequest};
use pond_core::user_data::services::onboarding::OnboardingService;
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

use pond_core::models::domain::model_record::{ModelCategory, ModelRecord, ModelRoleAssignment};
use pond_core::user_data::domain::memory::MemoryFragment;
use pond_core::user_data::domain::prompt_extra::PromptExtra;
use pond_core::user_data::domain::prompt_template::PromptTemplate;
use pond_core::user_data::domain::recipe::AgentRecipe;
use pond_core::user_data::domain::skill::UserSkill;

use crate::middleware::onboarding_guard::require_onboarding_complete;
use crate::{AppState, DownloadEntry, ModelStatusEntry};

// ───────────────────────── REST API Routes ─────────────────────────

/// Builds the full REST API router with onboarding-aware middleware
pub fn api_routes(state: Arc<AppState>) -> Router<Arc<AppState>> {
    // ───────────── Public routes (accessible before onboarding) ─────────────
    let public_routes = Router::new()
        .route("/health", get(health))
        .route("/handshake", post(handshake_handler))
        // Two-phase pairing (#93): init → verify, plus refresh / revoke and a
        // loopback-only endpoint to re-display the current pairing code.
        .route("/handshake/init", post(handshake_init))
        .route("/handshake/verify", post(handshake_verify))
        .route("/handshake/refresh", post(handshake_refresh))
        .route("/handshake/revoke", post(handshake_revoke))
        .route(
            "/handshake/pairing-code",
            get(handshake_pairing_code).post(handshake_issue_pairing_code),
        )
        .route("/onboard", post(start_onboarding))
        .route("/onboard/complete", post(complete_onboarding))
        .route("/onboard/status", get(onboarding_status))
        // Per-step progress tracking so the wizard can resume-from-N (public —
        // called mid-onboarding before completion).
        .route("/onboard/step/{name}", post(onboarding_step))
        // Reset onboarding back to the first step ("Start over" in Settings).
        .route("/onboard/reset", post(reset_onboarding))
        // Settings write is public so onboarding steps can save before completion
        .route("/settings", put(update_settings))
        // TTS synthesis is public so the onboarding voice-preview can play a
        // sample before onboarding completes. Text→audio via local Piper is not
        // privileged and leaks no user data.
        .route("/tts", post(tts_synthesise))
        // Transcription proxy (public — local test tool)
        .route("/transcribe", post(transcribe))
        // Wake-word phrase calibration (public — used during onboarding WakeWord step)
        .route("/voice/calibrate", post(calibrate_wake_word))
        .route("/voice/calibrate", delete(reset_wake_word_calibration))
        .route("/system/info", get(system_info))
        // Service connectivity test (public — diagnostic tool)
        .route("/test", get(test_services))
        .route("/test/speak", post(test_speak))
        // Goose agent status (public — dev diagnostic)
        .route("/dev/goose", get(goose_status))
        // Profile create/patch are public so onboarding steps can write before completion
        .route("/profiles", post(create_profile))
        .route("/profiles/{id}", patch(update_profile_prefs));

    // ───────────── Protected routes (require onboarding) ─────────────
    let protected_routes = Router::new()
        .route("/chat", post(chat))
        .route("/chat/stream", post(chat_stream))
        .route("/sessions", get(list_sessions))
        .route(
            "/sessions/{session_id}",
            patch(rename_session).delete(delete_session),
        )
        .route("/sessions/{session_id}/messages", get(get_session_messages))
        .route("/usage/summary", get(usage_summary))
        .route("/devices", get(list_devices).post(register_device))
<<<<<<< Updated upstream
        .route("/devices/{id}", axum::routing::delete(unregister_device))
=======
        .route("/devices/commission", post(commission_device))
        .route(
            "/devices/{id}",
            axum::routing::delete(unregister_device).put(update_device),
        )
>>>>>>> Stashed changes
        .route("/devices/{id}/heartbeat", post(device_heartbeat))
        .route("/devices/{id}/offline", post(device_offline))
        // Push-notification token register/unregister for a paired device (#95).
        .route(
            "/devices/{id}/push-token",
            post(register_push_token).delete(delete_push_token),
        )
        .route("/settings", get(get_settings))
        .route("/weather", get(get_weather))
        .route("/models", get(list_models))
        .route("/models/capabilities", get(get_model_capabilities))
        .route("/models/memory-status", get(get_memory_status))
        .route("/models/active-roles", get(get_active_roles))
        .route("/models/registry/refresh", post(refresh_model_registry))
        .route("/models/ollama", get(list_ollama_models))
        .route("/models/ollama/pull", post(pull_ollama_model))
        .route("/models/search/gguf", get(search_gguf_models))
        .route("/models/search/gguf/files", get(list_hf_model_files))
        .route("/models/search/llamafile", get(search_llamafile_models))
        .route("/models/download/url", post(download_model_from_url))
        .route("/models/download/progress", get(get_download_progress))
        .route("/models/scan", post(scan_models))
        .route("/models/cleanup", post(cleanup_models))
        .route("/models/disk-usage", get(disk_usage))
        .route("/models/{category}/{name}/download", post(download_model))
        .route("/models/{category}/{name}/activate", post(activate_model))
        .route("/models/{category}/{name}", delete(delete_model))
        .route("/profiles", get(list_profiles))
        .route("/profiles/{id}", get(get_profile).delete(delete_profile))
        .route("/sensors", post(record_sensor))
        .route("/sensors/{device_id}", get(get_recent_sensors))
        // Activity query API (#114) — read the unified event log.
        // DELETE clears activity on demand (#117, "clear my activity").
        .route("/activity", get(get_activity).delete(clear_activity))
        .route("/activity/summary", get(activity_summary))
        .route(
            "/camera/events",
            get(list_camera_events).post(record_camera_event),
        )
        .route(
            "/camera/events/{id}/acknowledge",
            patch(acknowledge_camera_event),
        )
        // ── Scheduler ──────────────────────────────────────────────────────────
        .route("/schedules", get(list_schedules).post(create_schedule))
        .route("/schedules/upcoming", get(list_upcoming_schedules))
        .route(
            "/schedules/{id}",
            delete(delete_schedule).put(update_schedule),
        )
        .route("/schedules/{id}/pause", post(pause_schedule))
        .route("/schedules/{id}/resume", post(resume_schedule))
        .route("/schedules/{id}/run-now", post(run_schedule_now))
        .route("/schedules/{id}/runs", get(list_schedule_runs))
        .route("/schedules/events", get(schedule_events_sse))
        // Foreground push: per-device notification stream (#99).
        .route("/notifications/stream", get(notifications_stream))
        // ── Extensions (MCP/Goose extension manager) ───────────────────────────
        .route(
            "/extensions",
            get(list_extensions_handler).post(add_extension_handler),
        )
        .route(
            "/extensions/{name}",
            delete(remove_extension_handler).patch(toggle_extension_handler),
        )
        // ── Secrets ────────────────────────────────────────────────────────────
        .route("/secrets", get(list_secrets_handler))
        .route(
            "/secrets/{key}",
            put(set_secret_handler).delete(delete_secret_handler),
        )
        .route("/secrets/{key}/exists", get(check_secret_handler))
        .route(
            "/extensions/{name}/secrets",
            get(get_extension_secrets_handler).post(set_extension_secrets_handler),
        )
        // ── OAuth PKCE ─────────────────────────────────────────────────────────
        .route("/oauth/authorize", post(oauth_authorize_handler))
        .route("/oauth/callback", get(oauth_callback_handler))
        .route("/oauth/refresh", post(oauth_refresh_handler))
        .route("/oauth/providers", get(oauth_providers_handler))
        // ── Music (Spotify) ────────────────────────────────────────────────────
        .route("/music/now-playing", get(music_now_playing_handler))
        .route("/music/control", post(music_control_handler))
        // ── Extension Marketplace ─────────────────────────────────────────────
        .route("/marketplace", get(list_marketplace_handler))
        .route(
            "/marketplace/{id}/install",
            post(install_marketplace_handler),
        )
        // ── Prompt Templates ───────────────────────────────────────────────────
        .route("/prompts", get(list_prompt_templates))
        .route(
            "/prompts/{name}",
            get(get_prompt_template)
                .put(upsert_prompt_template)
                .delete(delete_prompt_template),
        )
        .route("/prompts/{name}/reset", post(reset_prompt_template))
        // ── Agent Tools (MCP) ─────────────────────────────────────────────────
        .route("/agent/tools", get(list_agent_tools))
        // ── Agent chat stream (agentic tool-use loop) ─────────────────────────
        .route("/agent/chat/stream", post(agent_chat_stream))
        // ── MCP App Resources ────────────────────────────────────────────────
        .route("/mcp/resources", get(mcp_read_resource))
        .route("/mcp/tools/call", post(mcp_call_tool))
        .route("/tools/invoke", post(invoke_tool))
        // ── System Prompt Extras ───────────────────────────────────────────────
        .route(
            "/agent/extras",
            get(list_prompt_extras).post(upsert_prompt_extra),
        )
        .route("/agent/extras/{key}", delete(delete_prompt_extra))
        // ── Turn-level telemetry ──────────────────────────────────────────────
        .route("/telemetry/turns", get(get_telemetry_turns))
        .route("/telemetry/summary", get(get_telemetry_summary))
        // ── Event log / telemetry ─────────────────────────────────────────────
        .route("/logs", get(list_logs))
        .route("/logs/export", get(export_logs_csv))
        // ── Memories ──────────────────────────────────────────────────────────
        .route("/memories", get(list_memories).post(save_memory))
        .route("/memories/{id}", delete(delete_memory))
        // ── Memory Consolidation ─────────────────────────────────────────────
        .route("/memory/consolidate", post(start_consolidation))
        .route("/memory/consolidate/stop", post(stop_consolidation))
        // ── Skills ────────────────────────────────────────────────────────────
        .route("/skills", get(list_skills).post(create_skill))
        .route("/skills/{id}", put(update_skill).delete(delete_skill))
        // ── Recipes ───────────────────────────────────────────────────────────
        .route("/recipes", get(list_recipes).post(create_recipe))
        .route("/recipes/{id}", put(update_recipe).delete(delete_recipe))
        .route("/recipes/{name}/run", post(run_recipe))
        // ── Face biometrics (Phase 2) ─────────────────────────────────────────
        .route("/faces/register", post(register_face_handler))
        .route("/faces/identify", post(identify_face_handler))
        .route("/faces/identify-burst", post(burst_identify_face_handler))
        .route("/faces/enroll-quality", post(enroll_quality_handler))
        .route("/faces/profile/{profile_id}", get(list_face_enrollments))
        .route(
            "/faces/profile/{profile_id}/threshold",
            get(get_profile_threshold_handler)
                .put(put_profile_threshold_handler)
                .delete(delete_profile_threshold_handler),
        )
        .route("/faces/debug/pairwise", get(face_pairwise_debug))
        .route("/faces/debug/eval", get(face_eval_debug))
        .route("/faces/models", get(list_face_models_handler))
        .route(
            "/users/{profile_id}/biometrics",
            delete(delete_user_biometrics),
        )
        // Wake-on-face: bind an identified profile to an active chat session
        .route(
            "/sessions/{session_id}/identify-user",
            post(identify_session_user_handler),
        )
        .route(
            "/sessions/{session_id}/user",
            get(get_session_user_handler).delete(clear_session_user_handler),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_onboarding_complete,
        ));

    // Merge public and protected routes, attach shared state
    public_routes.merge(protected_routes).with_state(state)
}

// ───────────────────────── Web Dashboard Routes ─────────────────────

/// The web UI, embedded into the binary at compile time (single-executable).
/// Populated by `build.rs` + `vite build`; a placeholder until the real UI is
/// built (detected via the `data-giap-placeholder` marker below).
static WEB_DIST: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../pond-desktop/dist");

/// True when a *real* built UI is embedded (not the build.rs placeholder).
pub(crate) fn embedded_ui_present() -> bool {
    WEB_DIST
        .get_file("index.html")
        .map(|f| {
            !f.contents()
                .windows(20)
                .any(|w| w == b"data-giap-placeholder")
        })
        .unwrap_or(false)
}

fn file_response(path: &str, bytes: Vec<u8>) -> Response<axum::body::Body> {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    (
        [(axum::http::header::CONTENT_TYPE, mime.as_ref())],
        axum::body::Body::from(bytes),
    )
        .into_response()
}

/// Serve the web UI. Prefers the embedded bundle (single-executable); falls back
/// to the on-disk `static_dir` for dev builds where the UI wasn't embedded.
/// SPA-aware: unknown non-asset routes return `index.html`.
pub async fn serve_web(uri: axum::http::Uri, static_dir: std::path::PathBuf) -> impl IntoResponse {
    let raw = uri.path().trim_start_matches('/');
    let rel = if raw.is_empty() { "index.html" } else { raw };

    if embedded_ui_present() {
        if let Some(file) = WEB_DIST.get_file(rel) {
            return file_response(rel, file.contents().to_vec());
        }
        // SPA fallback: a route with no file extension → serve the app shell.
        if !rel.contains('.') {
            if let Some(index) = WEB_DIST.get_file("index.html") {
                return file_response("index.html", index.contents().to_vec());
            }
        }
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }

    // Dev fallback: read from the on-disk static dir (SPA fallback to index.html).
    serve_from_disk(&static_dir, rel).await
}

async fn serve_from_disk(static_dir: &std::path::Path, rel: &str) -> Response<axum::body::Body> {
    // Prevent path traversal: reject any candidate that escapes the root.
    let candidate = static_dir.join(rel);
    if candidate.starts_with(static_dir) {
        if let Ok(bytes) = tokio::fs::read(&candidate).await {
            return file_response(rel, bytes);
        }
    }
    if !rel.contains('.') {
        if let Ok(bytes) = tokio::fs::read(static_dir.join("index.html")).await {
            return file_response("index.html", bytes);
        }
    }
    (
        StatusCode::NOT_FOUND,
        "Web UI not available. Build it with `cd pond-desktop && npm run build`, \
         or pass --static-dir.",
    )
        .into_response()
}

// ───────────────────────── Handlers ─────────────────────────────────

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

/// Handshake endpoint to get authentication token (public)
///
/// TODO: Implement full GIAP ↔ GOTG handshake:
/// 1. Verify the GOTG client identity
/// 2. Exchange a session token
/// 3. Return connection details (hostname, port, capabilities)
async fn handshake_handler(
    State(state): State<Arc<AppState>>,
    body: Result<Json<HandshakeRequest>, JsonRejection>,
) -> Result<Json<HandshakeResponse>, (axum::http::StatusCode, Json<Value>)> {
    let Json(request) = body.map_err(|e| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("Invalid request: {}", e),
                "status": 400
            })),
        )
    })?;

    let response = state.handshake.handshake(request).await.map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("Handshake failed: {}", e),
                "status": 500
            })),
        )
    })?;

    Ok(Json(response))
}

/// Log an internal handshake error server-side and return a generic message,
/// so DB/internal error strings are never leaked to (untrusted) callers.
fn handshake_error(action: &str, e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    tracing::warn!(action, error = %e, "handshake request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "handshake request failed" })),
    )
}

/// Generic 400 for a malformed request body (carries no internal detail).
fn bad_body() -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "invalid request body" })),
    )
}

/// Phase 1 of pairing: client requests a challenge (public).
async fn handshake_init(
    State(state): State<Arc<AppState>>,
    body: Result<Json<InitRequest>, JsonRejection>,
) -> Result<Json<ChallengeResponse>, (StatusCode, Json<Value>)> {
    let Json(request) = body.map_err(|_| bad_body())?;
    let resp = state
        .handshake
        .init_handshake(request)
        .await
        .map_err(|e| handshake_error("init", e))?;
    Ok(Json(resp))
}

/// Dedicated, stricter per-IP limiter for `/handshake/verify` — the one
/// brute-forceable endpoint (an attacker guessing MACs). It complements the
/// single-use challenge (each guess burns a challenge, forcing a fresh,
/// rate-limited `init`). 10 attempts / 60 s is ample for legitimate pairing
/// (a client pairs once) while making online MAC-guessing hopeless.
fn verify_limiter() -> &'static crate::middleware::RateLimiter {
    static VERIFY_LIMITER: std::sync::OnceLock<crate::middleware::RateLimiter> =
        std::sync::OnceLock::new();
    VERIFY_LIMITER
        .get_or_init(|| crate::middleware::RateLimiter::new(10, std::time::Duration::from_secs(60)))
}

/// Record a pairing outcome in the unified event log (category `Auth`,
/// `Sensitive` — surfaceable by the audit tools, never the payload itself)
/// and push a security notification to connected devices (#164 follow-up).
/// Both are best-effort: they must never change the handshake response.
async fn emit_pairing_outcome(state: &AppState, paired: bool, device_name: Option<&str>) {
    use pond_core::security::domain::event::{Event, EventCategory, PrivacySensitivity};

    let action = if paired {
        "auth.device_paired"
    } else {
        "auth.pairing_verify_failed"
    };
    if let Some(event_log) = state.event_log.as_ref() {
        let mut event =
            Event::new(EventCategory::Auth, action).sensitivity(PrivacySensitivity::Sensitive);
        if let Some(name) = device_name {
            event = event.attr("device_name", name);
        }
        if let Err(e) = event_log.append(event).await {
            tracing::warn!(error = %e, action, "failed to record pairing event");
        }
    }

    let Some(sender) = state.notification_sender.as_ref() else {
        return;
    };
    // Debounce failure ALERTS (not the events above): a brute-force burst
    // should produce one phone alert per window, not one per guess.
    if !paired {
        static LAST_FAILURE_ALERT: std::sync::Mutex<Option<std::time::Instant>> =
            std::sync::Mutex::new(None);
        const FAILURE_ALERT_WINDOW: std::time::Duration = std::time::Duration::from_secs(600);
        let mut last = LAST_FAILURE_ALERT.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|at| at.elapsed() < FAILURE_ALERT_WINDOW) {
            return;
        }
        *last = Some(std::time::Instant::now());
    }

    let (category, title, body) = if paired {
        (
            "info",
            "New device paired".to_string(),
            format!(
                "\"{}\" was just paired with this Pond and can now access it.",
                device_name.unwrap_or("A new device")
            ),
        )
    } else {
        (
            "alert",
            "Failed pairing attempt".to_string(),
            "A device failed pairing verification. If this wasn't you, \
             issue a fresh pairing code."
                .to_string(),
        )
    };
    let notification = pond_core::mcp::ports::notification::Notification {
        id: uuid::Uuid::new_v4().to_string(),
        target: "broadcast".to_string(),
        category: category.to_string(),
        title,
        body,
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: None,
    };
    if let Err(e) = sender.broadcast(notification).await {
        tracing::warn!(error = %e, action, "failed to push pairing notification");
    }
}

/// Phase 2 of pairing: client proves the pairing code via MAC (public).
async fn handshake_verify(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    body: Result<Json<VerifyRequest>, JsonRejection>,
) -> Result<Json<HandshakeResponse>, (StatusCode, Json<Value>)> {
    // Rate-limit verify attempts per source IP (applies to loopback too — this
    // endpoint is security-sensitive regardless of origin).
    if !verify_limiter()
        .check_rate_limit(&peer.ip().to_string())
        .await
    {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": "too many handshake attempts; slow down"})),
        ));
    }
    let Json(request) = body.map_err(|_| bad_body())?;
    let device_name = request.device_name.clone();
    let resp = match state.handshake.verify_handshake(request).await {
        Ok(resp) => resp,
        Err(e) => {
            emit_pairing_outcome(&state, false, device_name.as_deref()).await;
            return Err(handshake_error("verify", e));
        }
    };
    emit_pairing_outcome(&state, resp.accepted, device_name.as_deref()).await;
    Ok(Json(resp))
}

/// Exchange a refresh token for a fresh session+refresh pair (public).
async fn handshake_refresh(
    State(state): State<Arc<AppState>>,
    body: Result<Json<RefreshRequest>, JsonRejection>,
) -> Result<Json<HandshakeResponse>, (StatusCode, Json<Value>)> {
    let Json(request) = body.map_err(|_| bad_body())?;
    let resp = state
        .handshake
        .refresh(request)
        .await
        .map_err(|e| handshake_error("refresh", e))?;
    Ok(Json(resp))
}

#[derive(Deserialize)]
struct RevokeRequest {
    token: String,
}

/// Revoke a session token — i.e. log the device out (public).
async fn handshake_revoke(
    State(state): State<Arc<AppState>>,
    body: Result<Json<RevokeRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(request) = body.map_err(|_| bad_body())?;
    state
        .handshake
        .revoke_token(&request.token)
        .await
        .map_err(|e| handshake_error("revoke", e))?;
    Ok(Json(json!({"revoked": true})))
}

/// Re-display the current pairing code. **Loopback-only** — the operator's own
/// machine (CLI/desktop dashboard), never a remote client.
async fn handshake_pairing_code(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !peer.ip().is_loopback() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "pairing code is only viewable on the host"})),
        ));
    }
    let code = state
        .handshake
        .current_pairing_code()
        .await
        .map_err(|e| handshake_error("pairing_code_lookup", e))?;
    match code {
        Some(pc) => Ok(Json(json!({"code": pc.code, "expires_at": pc.expires_at}))),
        None => Ok(Json(json!({"code": null}))),
    }
}

/// Issue a **fresh** single-use pairing code. **Loopback-only** — this is the
/// "pair a new device" action the operator triggers from the host (CLI/desktop
/// dashboard) to pair an additional phone after the startup code is consumed.
async fn handshake_issue_pairing_code(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !peer.ip().is_loopback() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "pairing codes can only be issued on the host"})),
        ));
    }
    let pc = state
        .handshake
        .issue_pairing_code()
        .await
        .map_err(|e| handshake_error("issue_pairing_code", e))?;
    Ok(Json(json!({"code": pc.code, "expires_at": pc.expires_at})))
}

/// Start or report onboarding state (public)
async fn start_onboarding(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let service = OnboardingService::new(state.onboarding_repo.clone());

    match service.status().await {
        Some(OnboardingStep::Completed) => Ok(Json(json!({
            "status": "already_complete",
            "message": "Onboarding has already been completed"
        }))),
        Some(step) => Ok(Json(json!({
            "status": "in_progress",
            "message": "Onboarding already started",
            "current_step": step.to_string()
        }))),
        None => {
            service.start().await.map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("Failed to start onboarding: {}", e)})),
                )
            })?;
            Ok(Json(json!({
                "status": "started",
                "current_step": OnboardingStep::Welcome.to_string()
            })))
        }
    }
}

/// Mark onboarding as complete (public).
///
/// Called by the web UI on the final onboarding step. Saving
/// `OnboardingStep::Completed` lifts the onboarding guard middleware so that
/// protected routes become accessible. This must be called and must succeed
/// before the client attempts any authenticated request; if it fails the UI
/// shows an error and does not navigate to the dashboard.
async fn complete_onboarding(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Validate the minimum required configuration before finishing. A half-set-up
    // assistant (no name, no timezone, or no chat model) must not lift the
    // onboarding guard — it would leave the user in a broken dashboard.
    let settings = state.settings_repo.get().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to load settings: {}", e)})),
        )
    })?;

    let mut missing: Vec<&str> = Vec::new();
    if settings.user_name.trim().is_empty() {
        missing.push("user_name");
    }
    if settings.assistant_name.trim().is_empty() {
        missing.push("assistant_name");
    }
    if settings.timezone.trim().is_empty() {
        missing.push("timezone");
    }
    if settings.chat_model.trim().is_empty() {
        missing.push("chat_model");
    }
    if !missing.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Onboarding is incomplete — required settings are missing.",
                "missing_fields": missing,
            })),
        ));
    }

    state
        .onboarding_repo
        .save_step(OnboardingStep::Completed)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to complete onboarding: {}", e)})),
            )
        })?;
    Ok(Json(json!({"status": "completed"})))
}

/// Build the canonical onboarding-status JSON from the persisted current step.
///
/// Shared by `GET /onboard/status`, `POST /onboard/step/:name`, and
/// `POST /onboard/reset` so every response has the same shape. Counts are
/// derived from [`OnboardingStep::ALL`] so they can never drift from the enum.
fn onboarding_status_json(step: Option<OnboardingStep>) -> Value {
    let total_steps = OnboardingStep::ALL.len();
    let (current_step, steps_completed, onboarded) = match step {
        None => ("not_started".to_string(), 0, false),
        Some(step) => (step.to_string(), step.position(), step.is_complete()),
    };

    json!({
        "onboarded": onboarded,
        "current_step": current_step,
        "steps_completed": steps_completed,
        "total_steps": total_steps
    })
}

/// Return current onboarding progress (public)
async fn onboarding_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let service = OnboardingService::new(state.onboarding_repo.clone());
    Json(onboarding_status_json(service.status().await))
}

/// Record that the client has reached a named onboarding step (public).
///
/// `POST /api/v1/onboard/step/:name` — the wizard calls this as each step is
/// reached so the backend tracks progress and can resume-from-N if the user
/// quits mid-onboarding. `:name` is an [`OnboardingStep`] variant name
/// (e.g. `Basics`, `WakeWord`).
///
/// Progress is **monotonic**: the persisted step only ever moves forward. If
/// the client re-POSTs an earlier step (e.g. after navigating Back), the
/// furthest-reached step is retained. `Completed` is rejected here — completion
/// is owned by `/onboard/complete`, which validates required settings first.
async fn onboarding_step(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let requested = OnboardingStep::from_str(&name).map_err(|_| {
        let valid: Vec<String> = OnboardingStep::ALL
            .iter()
            .filter(|s| !s.is_complete())
            .map(|s| s.to_string())
            .collect();
        (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("Unknown onboarding step: {name}"),
                "valid_steps": valid,
            })),
        )
    })?;

    // Completion goes through `/onboard/complete` (with required-field
    // validation). Accepting it here would lift the onboarding guard
    // unvalidated.
    if requested.is_complete() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Use POST /onboard/complete to finish onboarding",
            })),
        ));
    }

    let service = OnboardingService::new(state.onboarding_repo.clone());

    // Monotonic: `advance_to` never regresses past the furthest step reached.
    let target = service.advance_to(requested).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save onboarding step: {}", e)})),
        )
    })?;

    Ok(Json(onboarding_status_json(Some(target))))
}

/// Reset onboarding back to the first step (public).
///
/// `POST /api/v1/onboard/reset` — powers the "Start over" control in Settings.
/// Clears any persisted progress and re-arms the onboarding guard so the wizard
/// is shown again from `Welcome`. Distinct from `POST /onboard` (which only
/// *starts* onboarding when no state exists and otherwise reports status).
async fn reset_onboarding(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let service = OnboardingService::new(state.onboarding_repo.clone());

    service.reset().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to reset onboarding: {}", e)})),
        )
    })?;
    service.start().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to restart onboarding: {}", e)})),
        )
    })?;

    Ok(Json(onboarding_status_json(Some(OnboardingStep::Welcome))))
}

#[derive(Deserialize)]
struct ChatRequest {
    session_id: Option<String>,
    message: String,
    /// Optional image attachments for multimodal models (base64-encoded).
    #[serde(default)]
    images: Vec<pond_core::models::domain::message::ImageAttachment>,
    /// When true, disable thinking and use voice-friendly responses.
    #[serde(default)]
    voice_mode: bool,
    /// When true, the request originates from Canvas mode. The LLM should
    /// always prefer tool calls so results render as visual cards.
    #[serde(default)]
    canvas_mode: bool,
}

/// Send a message and get a response.
///
/// Creates a new session if `session_id` is not provided.
/// Persists both user and assistant messages to session storage.
async fn chat(
    State(state): State<Arc<AppState>>,
    body: Result<Json<ChatRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;

    let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let storage = &state.session_storage;

    // Ensure session exists
    if storage.get_session(&session_id).await.is_err() {
        storage
            .create_session(session_id.clone())
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("Failed to create session: {}", e)})),
                )
            })?;
    }

    let model_role = "chat";

    // Build ChatService — agent is always primary (GooseAdapter builds system
    // prompt from DB settings, manages history, handles MCP tools internally).
    let service = ChatService::new(state.agent.clone(), session_id.clone(), storage.clone());

    let response_text = service.chat_once(req.message).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;

    Ok(Json(json!({
        "session_id": session_id,
        "response":   response_text,
        "model_role": model_role,
    })))
}

// ── TTS request ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TtsRequest {
    text: String,
}

/// Synthesise speech server-side and return WAV audio bytes.
///
/// Priority order:
/// 1. Piper HTTP server (if running — see `AppState.piper_http_port`)
async fn tts_synthesise(
    State(state): State<Arc<AppState>>,
    body: Result<Json<TtsRequest>, JsonRejection>,
) -> Result<Response<Body>, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;

    let text = req.text.trim();
    if text.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "'text' must not be empty"})),
        ));
    }

    let Some(port) = state.piper_http_port else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "No TTS backend is running"})),
        ));
    };

    let piper_url = format!("http://127.0.0.1:{}/tts", port);
    let resp = state
        .http_client
        .post(&piper_url)
        .header("content-type", "text/plain; charset=utf-8")
        .body(text.to_string())
        .send()
        .await
        .map_err(|e| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": format!("Piper HTTP unavailable: {}", e)})),
            )
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": format!(
                    "Piper HTTP synthesis failed (status {}): {}",
                    status.as_u16(),
                    body
                )
            })),
        ));
    }

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("audio/wav")
        .to_string();

    let bytes = resp.bytes().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("Failed reading Piper audio response: {}", e)})),
        )
    })?;

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", content_type)
        .body(Body::from(bytes.to_vec()))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to build TTS response: {}", e)})),
            )
        })
}

fn render_tool_guidance_from_extensions(extensions: &[ExtensionInfo]) -> String {
    if extensions.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        "## Tool Use Guidance".to_string(),
        "You may call tools when they are necessary to complete the user request.".to_string(),
        "- Prefer the smallest number of tool calls that can complete the task.".to_string(),
        "- If a tool fails, explain what failed and continue with the best possible answer."
            .to_string(),
        "".to_string(),
        "Available tools:".to_string(),
    ];

    let mut tool_count = 0usize;
    for ext in extensions {
        if ext.tools.is_empty() {
            continue;
        }
        tool_count += ext.tools.len();

        if ext.description.trim().is_empty() {
            lines.push(format!("- {} ({})", ext.name, ext.kind));
        } else {
            lines.push(format!(
                "- {} ({}) - {}",
                ext.name,
                ext.kind,
                ext.description.trim()
            ));
        }

        for tool in &ext.tools {
            lines.push(format!("  - {}", tool));
        }
    }

    if tool_count == 0 {
        return String::new();
    }

    lines.join("\n")
}

// ── SSE streaming chat ────────────────────────────────────────────────────────

/// Stream chat tokens via Server-Sent Events.
///
/// Each SSE event carries a JSON payload:
/// - Token event:  `data: {"token": "..."}`
/// - Done event:   `data: {"done": true, "session_id": "...", "model_role": "..."}`
/// - Error event:  `data: {"error": "..."}`
///
/// The stream persists both the user message and the full assistant response
/// to session storage before yielding the final done event.
async fn chat_stream(
    State(state): State<Arc<AppState>>,
    body: Result<Json<ChatRequest>, JsonRejection>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)>
{
    // Update activity timestamp — resets the consolidation inactivity timer
    *state.last_user_activity.write().await = std::time::Instant::now();
    // Cancel any in-progress consolidation
    if let Some(cancel) = state.consolidation_cancel.read().await.as_ref() {
        cancel.cancel();
    }

    let permit = state
        .sse_semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "Too many concurrent streams"})),
            )
        })?;
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
    })?;

    Ok(chat_stream_inner(state, permit, req))
}

/// Shared SSE pipeline used by both `chat_stream` and `run_recipe`.
///
/// Callers handle activity touching, body parsing, and semaphore acquisition;
/// this helper owns the full agent turn — session creation, system-prompt
/// build, llamafile startup wait, ThoughtFilter, telemetry, memory extraction —
/// and emits the same SSE event shape regardless of entry point.
fn chat_stream_inner(
    state: Arc<AppState>,
    permit: tokio::sync::OwnedSemaphorePermit,
    req: ChatRequest,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    use futures::StreamExt;
    use pond_core::models::ports::agent::AgentStreamEvent;

    let stream = async_stream::stream! {
        let _permit = permit;
        let turn_start = std::time::Instant::now();
        let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let storage = &state.session_storage;

        // Ensure session exists
        if storage.get_session(&session_id).await.is_err() {
            if let Err(e) = storage.create_session(session_id.clone()).await {
                let data = json!({"error": format!("Failed to create session: {}", e)}).to_string();
                yield Ok(Event::default().data(data));
                return;
            }
        }

        let settings = state.settings_repo.get().await.unwrap_or_default();

        // Build the system prompt
        let system_prompt = {
            let profile_ctx: Option<ProfileContext> = if let Some(ref pid) = settings.primary_profile_id {
                state.profile_repo.get(pid).await.ok().flatten().map(|p| {
                    let prefs = &p.preferences;
                    ProfileContext {
                        preferred_name: prefs.get("preferred_name").cloned(),
                        birthday: prefs.get("birthday").cloned(),
                        language: prefs.get("language").cloned(),
                        atypical_speech: prefs.get("accessibility_atypical_speech")
                            .map(|v| v == "true")
                            .unwrap_or(false),
                    }
                })
            } else {
                None
            };

            let file_template = match state.prompt_template_dir.as_ref() {
                Some(dir) => tokio::fs::read_to_string(dir.join("system.md")).await.ok(),
                None => None,
            };

            match file_template {
                Some(tmpl) => {
                    let name     = sanitize_field(&settings.assistant_name, 50);
                    let user     = sanitize_field(&settings.user_name, 50);
                    let persona  = sanitize_field(&settings.assistant_personality, 200);
                    let tz       = sanitize_field(&settings.timezone, 50);
                    let location = if settings.weather_location_name.is_empty() {
                        String::new()
                    } else {
                        format!("\nLocation: {}.", sanitize_field(&settings.weather_location_name, 100))
                    };
                    let addendum = sanitize_field(&settings.prompt_addendum, 500);
                    render_template(&tmpl, &[
                        ("assistant_name",  name.as_str()),
                        ("user_name",       user.as_str()),
                        ("personality",     persona.as_str()),
                        ("timezone",        tz.as_str()),
                        ("location",        location.as_str()),
                        ("prompt_addendum", addendum.as_str()),
                    ])
                }
                None => build_system_prompt_with_profile(&settings, profile_ctx.as_ref()),
            }
        };

        let mut _system_prompt = match &state.mcp_memory {
            Some(m) => {
                let mem = m.instructions();
                if mem.is_empty() { system_prompt } else { format!("{}\n\n---\n{}", system_prompt, mem) }
            }
            None => system_prompt,
        };

        if let Some(mgr) = &state.extension_manager {
            match mgr.list_extensions().await {
                Ok(extensions) => {
                    const MAX_TOOL_GUIDANCE_CHARS: usize = 4_000;
                    const TOOL_GUIDANCE_TRUNCATION_NOTE: &str =
                        "\n\n[tool guidance truncated; additional tools omitted]";

                    let guidance = render_tool_guidance_from_extensions(&extensions);
                    if !guidance.is_empty() {
                        let bounded_guidance = if guidance.len() > MAX_TOOL_GUIDANCE_CHARS {
                            let reserved = TOOL_GUIDANCE_TRUNCATION_NOTE.len();
                            let max_content_len = MAX_TOOL_GUIDANCE_CHARS.saturating_sub(reserved);
                            let mut cut = max_content_len.min(guidance.len());
                            while cut > 0 && !guidance.is_char_boundary(cut) {
                                cut -= 1;
                            }
                            format!("{}{}", &guidance[..cut], TOOL_GUIDANCE_TRUNCATION_NOTE)
                        } else {
                            guidance
                        };

                        _system_prompt.push_str("\n\n");
                        _system_prompt.push_str(&bounded_guidance);
                    }
                }
                Err(e) => {
                    tracing::debug!("Failed to list extensions for tool guidance: {}", e);
                }
            }
        }

        let model_role = "chat";

        let mut chat_service = pond_core::shared::services::chat::ChatService::new(
            state.agent.clone(),
            session_id.clone(),
            storage.clone(),
        );
        if let (Some(ext), Some(svc)) =
            (state.memory_extractor.clone(), state.memory_extraction_service.clone())
        {
            chat_service = chat_service.with_memory_extraction(
                ext,
                svc,
                state.memory_repo.clone(),
            );
        }

        // ── Persist user message ────────────────────────────────────────────
        if let Err(e) = chat_service.persist_user_message(&req.message).await {
            let data = json!({"error": format!("Failed to persist user message: {}", e)}).to_string();
            yield Ok(Event::default().data(data));
            return;
        }

        // ── On-demand llamafile startup ─────────────────────────────────────
        // If any role uses llamafile and the process is not responding, emit a
        // status event and wait up to 90 s before attempting to stream.
        {
            let is_llamafile_role = settings.chat_provider == "llamafile";

            if is_llamafile_role {
                if let Some(manager) = &state.llamafile_manager {
                    if !manager.is_running().await {
                        let status = json!({"type": "status", "content": "Model starting…"})
                            .to_string();
                        yield Ok(Event::default().data(status));

                        let model_hint = if settings.chat_provider == "llamafile" {
                            Some(settings.chat_model.as_str())
                        } else {
                            None
                        };

                        let (_url, ready) = manager
                            .ensure_started_and_wait(model_hint, 90)
                            .await;

                        if !ready {
                            let data = json!({"error":
                                "llamafile did not start within 90 s — \
                                 check that a model file is installed"
                            }).to_string();
                            yield Ok(Event::default().data(data));
                            return;
                        }
                    }
                }
            }
        }

        let mut usage_prompt_tokens: u32 = 0;
        let mut usage_completion_tokens: u32 = 0;

        let model_name_for_done = settings.chat_model.clone();

        use pond_core::shared::domain::agent::AgentRequest;

        let agent_req = AgentRequest {
            message: req.message.clone(),
            session_id: session_id.clone(),
            model_role: model_role.to_string(),
            images: req.images.clone(),
            voice_mode: req.voice_mode,
            canvas_mode: req.canvas_mode,
        };

        let mut full_text = String::new();
        let mut tool_results: Vec<String> = Vec::new();
        let mut ttft_instant: Option<std::time::Instant> = None;
        // Tracks the most recent tool call's name and wall-clock latency, used
        // to populate per-turn telemetry below. When a turn invokes several
        // tools, only the last one is recorded — TurnMetrics has a single slot.
        let mut last_tool_name: Option<String> = None;
        let mut last_tool_latency_ms: Option<u64> = None;
        let mut tool_call_start: Option<std::time::Instant> = None;
        // Filter Harmony-style `<|channel>thought ... <channel|>` reasoning
        // preambles and `<think>…</think>` blocks out of the per-token stream.
        // When show_thinking is enabled, capture thinking blocks as SSE events.
        // Voice mode always disables thinking capture.
        let mut thought = if settings.show_thinking && !req.voice_mode {
            crate::thought_filter::ThoughtFilter::new().with_thinking_capture()
        } else {
            crate::thought_filter::ThoughtFilter::new()
        };
        let mut agent_stream = match state.agent.chat_stream(agent_req).await {
            Ok(s) => s,
            Err(e) => {
                let data = json!({"error": e.to_string()}).to_string();
                yield Ok(Event::default().data(data));
                return;
            }
        };

        // ── Agent turn idle timeout ────────────────────────────────────
        // Bound the SILENCE between stream events, not total generation.
        // A slow reasoning model that streams continuously must never be
        // killed; only a genuinely stalled stream (no event for
        // `agent_timeout_secs`) trips the deadline. The deadline is reset
        // after every event received below. When agent_timeout_secs is 0
        // the timeout is disabled (24h sentinel keeps the path uniform).
        let timeout_secs = settings.agent_timeout_secs;
        let idle_budget = if timeout_secs == 0 {
            std::time::Duration::from_secs(86_400) // effectively disabled
        } else {
            std::time::Duration::from_secs(timeout_secs)
        };
        let mut deadline = tokio::time::Instant::now() + idle_budget;
        let mut timed_out = false;

        loop {
            match tokio::time::timeout_at(deadline, agent_stream.next()).await {
                Ok(Some(event_result)) => {
                    // Progress observed — extend the idle window.
                    deadline = tokio::time::Instant::now() + idle_budget;
                    match event_result {
                        Ok(event) => {
                            let maybe_data = match event {
                                AgentStreamEvent::Status { content } => {
                                    Some(json!({"type": "status", "content": content}).to_string())
                                }
                                AgentStreamEvent::Thinking { content } => {
                                    Some(json!({"type": "thinking", "content": content}).to_string())
                                }
                                AgentStreamEvent::ToolCall { tool, id, input } => {
                                    tool_call_start = Some(std::time::Instant::now());
                                    last_tool_name = Some(tool.clone());
                                    Some(json!({"type": "tool_call", "tool": tool, "id": id, "input": input}).to_string())
                                }
                                AgentStreamEvent::ToolResult { tool, id, content } => {
                                    if let Some(start) = tool_call_start.take() {
                                        last_tool_latency_ms = Some(start.elapsed().as_millis() as u64);
                                    }
                                    let (clean_content, ui_hint) = extract_ui_hint(&content);
                                    let mut ev = serde_json::json!({
                                        "type": "tool_result",
                                        "tool": tool,
                                        "id": id,
                                        "content": clean_content
                                    });
                                    if let Some(ui) = ui_hint {
                                        ev["ui"] = ui;
                                    }
                                    tool_results.push(json!({
                                        "tool_call_id": id,
                                        "tool": tool,
                                        "content": clean_content,
                                    }).to_string());
                                    Some(ev.to_string())
                                }
                                AgentStreamEvent::Text { content } => {
                                    let visible = thought.push(&content);
                                    if visible.is_empty() {
                                        None
                                    } else {
                                        if ttft_instant.is_none() {
                                            ttft_instant = Some(std::time::Instant::now());
                                        }
                                        full_text.push_str(&visible);
                                        Some(json!({"type": "text", "content": visible, "token": visible}).to_string())
                                    }
                                }
                                AgentStreamEvent::ReviewStatus { content } => {
                                    Some(json!({"type": "review_status", "content": content}).to_string())
                                }
                                AgentStreamEvent::ReviewRevision { content, score, rounds } => {
                                    Some(json!({"type": "review_revision", "content": content, "score": score, "rounds": rounds}).to_string())
                                }
                                AgentStreamEvent::Done { usage, .. } => {
                                    if let Some(u) = usage {
                                        usage_prompt_tokens = u.prompt_tokens;
                                        usage_completion_tokens = u.completion_tokens;
                                    }
                                    continue;
                                }
                                AgentStreamEvent::Error { content } => {
                                    Some(json!({"error": content}).to_string())
                                }
                            };
                            if let Some(data) = maybe_data {
                                yield Ok(Event::default().data(data));
                            }
                            // Emit captured thinking blocks as SSE events (when show_thinking is on)
                            for thinking_content in thought.take_thinking() {
                                let data = json!({"type": "thinking", "content": thinking_content}).to_string();
                                yield Ok(Event::default().data(data));
                            }
                            // After every push the filter may have captured a complete
                            // tool-call envelope (`<|tool_call> ... <tool_call|>`).
                            // The model emitted tool calls as Harmony text markup instead of
                            // the structured protocol. Execute them directly as a fallback.
                            for body in thought.take_tool_calls() {
                                if let Some((name, args)) = crate::thought_filter::parse_tool_envelope(&body) {
                                    tracing::info!(tool = %name, "Executing text-based tool call (model used Harmony format)");
                                    let call_id = uuid::Uuid::new_v4().to_string();
                                    let args_val: serde_json::Value = serde_json::from_str(&args).unwrap_or(json!({}));
                                    yield Ok(Event::default().data(
                                        json!({"type": "tool_call", "tool": name.clone(), "id": call_id.clone(), "input": args_val}).to_string()
                                    ));
                                    let call_start = std::time::Instant::now();
                                    let call_result = state.agent.call_tool(&session_id, &name, &args).await;
                                    last_tool_name = Some(name.clone());
                                    last_tool_latency_ms = Some(call_start.elapsed().as_millis() as u64);
                                    match call_result {
                                        Ok(result_text) => {
                                            let (clean, ui_hint) = extract_ui_hint(&result_text);
                                            let mut ev = json!({
                                                "type": "tool_result",
                                                "tool": name,
                                                "id": call_id,
                                                "content": clean
                                            });
                                            if let Some(ui) = ui_hint {
                                                ev["ui"] = ui;
                                            }
                                            yield Ok(Event::default().data(ev.to_string()));
                                        }
                                        Err(e) => {
                                            yield Ok(Event::default().data(
                                                json!({"type": "tool_result", "tool": name, "id": call_id, "content": format!("Tool error: {e}")}).to_string()
                                            ));
                                        }
                                    }
                                } else {
                                    tracing::warn!("Unrecognised tool-call envelope: {body}");
                                }
                            }
                        }
                        Err(e) => {
                            let err_msg = e.to_string();
                            let data = json!({"error": err_msg}).to_string();
                            yield Ok(Event::default().data(data));
                            return;
                        }
                    }
                }
                Ok(None) => {
                    // Stream ended normally
                    break;
                }
                Err(_elapsed) => {
                    // Deadline exceeded — emit timeout error
                    timed_out = true;
                    tracing::warn!(
                        session_id = %session_id,
                        timeout_secs = timeout_secs,
                        "Agent turn timed out"
                    );
                    let data = json!({"error": "Agent timed out. Try a shorter message or start a new session."}).to_string();
                    yield Ok(Event::default().data(data));
                    break;
                }
            }
        }

        // Flush any tail buffered by the thought filter (e.g. text after the
        // last `<channel|>` that had not yet exceeded the safe-emit threshold).
        if !timed_out {
            let tail = thought.flush();
            if !tail.is_empty() {
                full_text.push_str(&tail);
                let data = json!({"type": "text", "content": tail, "token": tail}).to_string();
                yield Ok(Event::default().data(data));
            }
        }

        // ── Adversarial answer review (post-inference) ───────────────
        // When review_mode is "on", evaluate the answer before persisting.
        // If the reviewer rejects it, revise and emit a review_revision
        // event that the frontend uses to replace the text.
        // Skipped when the agent timed out — no point reviewing a partial answer.
        {
            let should_review = !timed_out && match settings.review_mode.as_str() {
                "on" => true,
                "auto" => {
                    let msg = req.message.to_lowercase();
                    msg.contains('?')
                        || msg.starts_with("what ")
                        || msg.starts_with("how ")
                        || msg.starts_with("why ")
                        || msg.starts_with("explain ")
                        || msg.starts_with("compare ")
                        || msg.starts_with("analyze ")
                }
                _ => false,
            };

            if should_review {
                if let Some(ref reviewer) = state.answer_reviewer {
                    let status = json!({"type": "review_status", "content": "Reviewing answer..."}).to_string();
                    yield Ok(Event::default().data(status));

                    match reviewer.review(&req.message, &full_text, None).await {
                        Ok(result) if result.was_revised => {
                            full_text = result.final_answer.clone();
                            let data = json!({
                                "type": "review_revision",
                                "content": result.final_answer,
                                "score": result.verdict.score,
                                "rounds": result.rounds,
                            }).to_string();
                            yield Ok(Event::default().data(data));
                        }
                        Ok(result) => {
                            let status = json!({
                                "type": "review_status",
                                "content": format!("Answer verified (score: {}/5)", result.verdict.score),
                            }).to_string();
                            yield Ok(Event::default().data(status));
                        }
                        Err(e) => {
                            tracing::warn!("Answer review failed (non-fatal): {}", e);
                        }
                    }
                }
            }
        }

        // ── Persist assistant turn + memory extraction ────────────────────
        // `persist_assistant_turn_with_extraction` owns both concerns: it
        // writes tool results / assistant text / usage to session_messages,
        // then spawns memory extraction in the background. The handler cannot
        // accidentally omit extraction by refactoring this block.
        let _ = chat_service
            .persist_assistant_turn_with_extraction(
                tool_results,
                &full_text,
                Some((usage_prompt_tokens, usage_completion_tokens)),
                Some(&model_name_for_done),
                &req.message,
            )
            .await;

        // ── Per-turn telemetry ──────────────────────────────────────────
        if settings.telemetry_enabled {
            if let Some(ref telemetry) = state.telemetry {
                let total_latency_ms = turn_start.elapsed().as_millis() as u64;
                let ttft_ms = ttft_instant
                    .map(|t| t.duration_since(turn_start).as_millis() as u64)
                    .unwrap_or(total_latency_ms);

                // Estimate turn number from existing telemetry for this session.
                let existing_turns = telemetry
                    .get_turns(&session_id)
                    .await
                    .map(|v| v.len() as u32)
                    .unwrap_or(0);

                let context_limit = if settings.context_window_override > 0 {
                    settings.context_window_override
                } else {
                    state.agent.capabilities().context_window_tokens
                };
                let estimated_tokens = usage_prompt_tokens + usage_completion_tokens;
                let context_utilization_pct = if context_limit > 0 {
                    (estimated_tokens as f32 / context_limit as f32) * 100.0
                } else {
                    0.0
                };

                let metrics = pond_core::security::domain::turn_metrics::TurnMetrics {
                    session_id: session_id.clone(),
                    turn_number: existing_turns + 1,
                    prompt_tokens: usage_prompt_tokens,
                    completion_tokens: usage_completion_tokens,
                    ttft_ms,
                    total_latency_ms,
                    tool_name: last_tool_name.clone(),
                    tool_latency_ms: last_tool_latency_ms,
                    tool_cache_hit: None,
                    context_utilization_pct,
                    model_name: model_name_for_done.clone(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                };

                if let Err(e) = telemetry.record_turn(metrics).await {
                    tracing::debug!(target: "giap::telemetry", "failed to record turn metrics: {e}");
                }
            }
        }

        // ── Context growth monitoring ─────────────────────────────────
        if settings.context_monitor_enabled {
            let estimated_tokens = usage_prompt_tokens + usage_completion_tokens;
            let context_limit = if settings.context_window_override > 0 {
                settings.context_window_override
            } else {
                let caps = state.agent.capabilities();
                caps.context_window_tokens
            };

            if estimated_tokens > 0 && context_limit > 0 {
                state.context_monitor.record_turn(
                    &session_id,
                    estimated_tokens,
                    context_limit,
                );

                let health = state.context_monitor.check_context_health(&session_id);

                if let Some(ref warning) = health.warning {
                    tracing::warn!(
                        session_id = %session_id,
                        utilization_pct = health.utilization_pct,
                        turns_remaining = health.estimated_turns_remaining,
                        "{}",
                        warning,
                    );
                }

                if health.should_compact {
                    let data = json!({
                        "type": "context_warning",
                        "utilization_pct": health.utilization_pct,
                        "turns_remaining": health.estimated_turns_remaining,
                        "avg_growth_rate": health.avg_growth_rate,
                        "warning": health.warning,
                    }).to_string();
                    yield Ok(Event::default().data(data));
                }
            }
        }

        // Done event
        let data = json!({
            "done": true,
            "session_id": session_id,
            "model_role": model_role,
            "model_name": model_name_for_done,
            "usage": {
                "prompt_tokens": usage_prompt_tokens,
                "completion_tokens": usage_completion_tokens,
            }
        }).to_string();
        yield Ok(Event::default().data(data));
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// List all sessions, ordered by most recently updated first.
async fn list_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let sessions = state.session_storage.list_sessions().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to list sessions: {}", e)})),
        )
    })?;

    let mut session_list: Vec<Value> = Vec::with_capacity(sessions.len());
    for s in &sessions {
        // Message count powers the sidebar badge. A failure here is non-fatal —
        // the badge just shows 0 rather than breaking the whole list.
        let message_count = state
            .session_storage
            .count_messages(&s.id)
            .await
            .unwrap_or(0);

        // Read-time title fallback: if a session has no stored title yet,
        // derive a short label from its first user message so the client
        // never has to render a raw session id. The stored title stays None —
        // this is a projection, not a mutation.
        let effective_title: Option<String> = match &s.title {
            Some(t) if !t.trim().is_empty() => Some(t.clone()),
            _ => state
                .session_storage
                .first_user_message(&s.id)
                .await
                .ok()
                .flatten()
                .map(|m| derived_session_label(&m))
                .filter(|t| !t.is_empty()),
        };

        session_list.push(json!({
            "id": s.id,
            "title": effective_title,
            "message_count": message_count,
            "total_prompt_tokens": s.total_prompt_tokens,
            "total_completion_tokens": s.total_completion_tokens,
            "model_name": s.model_name,
            "created_at": s.created_at.to_rfc3339(),
            "updated_at": s.updated_at.to_rfc3339(),
        }));
    }

    Ok(Json(json!({ "sessions": session_list })))
}

/// Derive a short, human-readable label from the first user message of a
/// session. Read-only helper for the sessions list fallback — collapses
/// whitespace and caps at ~40 characters on a char boundary.
fn derived_session_label(text: &str) -> String {
    const MAX_CHARS: usize = 40;
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = cleaned.trim_matches('"').trim_matches('\'').trim();
    if cleaned.chars().count() <= MAX_CHARS {
        cleaned.to_string()
    } else {
        let truncated: String = cleaned.chars().take(MAX_CHARS).collect();
        format!("{}…", truncated.trim_end())
    }
}

/// `GET /api/v1/usage/summary` — aggregate token usage across all sessions.
async fn usage_summary(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let sessions = state.session_storage.list_sessions().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to list sessions: {}", e)})),
        )
    })?;

    let total_prompt: u64 = sessions.iter().map(|s| s.total_prompt_tokens as u64).sum();
    let total_completion: u64 = sessions
        .iter()
        .map(|s| s.total_completion_tokens as u64)
        .sum();
    let total_tokens = total_prompt + total_completion;

    let settings = state.settings_repo.get().await.ok();
    let cloud_input = settings
        .as_ref()
        .map(|s| s.cloud_input_price_per_million)
        .unwrap_or(2.50);
    let cloud_output = settings
        .as_ref()
        .map(|s| s.cloud_output_price_per_million)
        .unwrap_or(10.00);

    Ok(Json(json!({
        "total_prompt_tokens": total_prompt,
        "total_completion_tokens": total_completion,
        "total_tokens": total_tokens,
        "session_count": sessions.len(),
        "cloud_input_price_per_million": cloud_input,
        "cloud_output_price_per_million": cloud_output,
    })))
}

// ── Telemetry endpoints ───────────────────────────────────────────────

#[derive(Deserialize)]
struct TelemetryQuery {
    session_id: String,
}

/// Return per-turn telemetry metrics for a session.
///
/// `GET /api/v1/telemetry/turns?session_id=X`
async fn get_telemetry_turns(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TelemetryQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let telemetry = state.telemetry.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Telemetry is not enabled"})),
        )
    })?;

    let turns = telemetry.get_turns(&query.session_id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to get turns: {}", e)})),
        )
    })?;

    Ok(Json(json!({ "turns": turns })))
}

/// Return an aggregated telemetry summary for a session.
///
/// `GET /api/v1/telemetry/summary?session_id=X`
async fn get_telemetry_summary(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TelemetryQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let telemetry = state.telemetry.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Telemetry is not enabled"})),
        )
    })?;

    let summary = telemetry
        .get_summary(&query.session_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to get summary: {}", e)})),
            )
        })?;

    Ok(Json(json!(summary)))
}

#[derive(Deserialize)]
struct RenameSessionRequest {
    title: String,
}

/// Rename a session (set or update its title).
///
/// PATCH /api/v1/sessions/:session_id
/// Body: { "title": "New Title" }
async fn rename_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    body: Result<Json<RenameSessionRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;

    state
        .session_storage
        .update_title(&session_id, req.title.clone())
        .await
        .map_err(|e| {
            let status = match &e {
                pond_core::user_data::ports::session_storage::SessionStorageError::SessionNotFound(_) => {
                    StatusCode::NOT_FOUND
                }
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({"error": format!("{}", e)})))
        })?;

    Ok(Json(json!({
        "session_id": session_id,
        "title": req.title,
    })))
}

/// Delete a session and all its messages.
///
/// DELETE /api/v1/sessions/:session_id
///
/// Idempotent: deleting a non-existent session returns 204 (the storage
/// layer does not distinguish a missing row from a deleted one).
async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    use pond_core::user_data::ports::session_storage::SessionStorageError;
    state
        .session_storage
        .delete_session(&session_id)
        .await
        .map_err(|e| {
            let status = match &e {
                SessionStorageError::SessionNotFound(_) => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({"error": format!("{}", e)})))
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Get messages for a session (paginated).
///
/// GET /api/v1/sessions/:session_id/messages?limit=100&offset=0
///
/// Query params (optional):
/// - `limit`:  max messages to return (default 100, capped at 500)
/// - `offset`: skip this many oldest messages (default 0)
///
/// When called without params, returns the 100 most recent messages — enough
/// for the UI to render a session without loading the full history into RAM.
async fn get_session_messages(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    use pond_core::models::domain::message::Role;
    use pond_core::user_data::ports::session_storage::SessionStorageError;

    let limit: usize = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100)
        .min(500);
    let offset: usize = params
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let messages = state
        .session_storage
        .get_messages_paginated(&session_id, limit, offset)
        .await
        .map_err(|e| {
            let status = match &e {
                SessionStorageError::SessionNotFound(_) => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({"error": format!("{}", e)})))
        })?;

    let list: Vec<Value> = messages
        .iter()
        .map(|m| {
            let role = match m.message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
                Role::Tool => "tool",
            };
            let mut obj = json!({
                "id": m.id,
                "session_id": m.session_id,
                "role": role,
                "content": m.message.content,
                "created_at": m.created_at.to_rfc3339(),
            });
            if let Some(tc_id) = &m.message.tool_call_id {
                obj["tool_call_id"] = json!(tc_id);
            }
            if !m.message.tool_calls.is_empty() {
                obj["tool_calls"] = json!(m
                    .message
                    .tool_calls
                    .iter()
                    .map(|tc| json!({
                        "id": tc.id,
                        "name": tc.name,
                        "arguments": tc.arguments,
                    }))
                    .collect::<Vec<_>>());
            }
            obj
        })
        .collect();

    Ok(Json(json!({ "messages": list })))
}

async fn system_info(State(state): State<Arc<AppState>>) -> Json<Value> {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let hostname = hostname
        .strip_suffix(".local")
        .unwrap_or(&hostname)
        .to_string();

    Json(json!({
        "hostname": hostname,
        "port": state.api_port,
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    }))
}

async fn list_devices(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let devices = state.device_registry.list_devices().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    let list: Vec<Value> = devices
        .iter()
        .map(|d| {
            json!({
                "id":            d.id,
                "name":          d.name,
                "device_type":   d.device_type,
                "hostname":      d.hostname,
                "ip_address":    d.ip_address,
                "capabilities":  d.capabilities,
                "registered_at": d.registered_at,
                "last_seen":     d.last_seen,
                "is_online":     d.is_online,
                "room":          d.room,
            })
        })
        .collect();
    Ok(Json(json!({ "devices": list })))
}

async fn register_device(
    State(state): State<Arc<AppState>>,
    body: Result<Json<RegisterDeviceRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;
    let device = state.device_registry.register(req).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id":            device.id,
            "name":          device.name,
            "device_type":   device.device_type,
            "capabilities":  device.capabilities,
            "registered_at": device.registered_at,
            "is_online":     device.is_online,
            "room":          device.room,
        })),
    ))
}

async fn unregister_device(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    state.device_registry.unregister(&id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn device_heartbeat(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state.device_registry.heartbeat(&id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    Ok(Json(json!({ "status": "ok" })))
}

/// `POST /api/v1/devices/{id}/offline` — the Devices UI's "Turn off" action.
/// Mirrors `device_heartbeat` ("Turn on"): devices without a real liveness
/// signal have no other way to report offline.
async fn device_offline(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state.device_registry.set_offline(&id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    Ok(Json(json!({ "status": "ok" })))
}

#[derive(serde::Deserialize)]
struct UpdateDeviceBody {
    name: String,
    hostname: Option<String>,
    room: Option<String>,
}

/// `PUT /api/v1/devices/{id}` — the Devices UI's "Configure" save action.
async fn update_device(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<UpdateDeviceBody>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "name is required"})),
        ));
    }
    let device = state
        .device_registry
        .update(
            &id,
            pond_core::user_data::ports::device_registry::UpdateDeviceRequest {
                name,
                hostname: req.hostname.filter(|s| !s.trim().is_empty()),
                room: req.room.filter(|s| !s.trim().is_empty()),
            },
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;
    Ok(Json(json!({
        "id":            device.id,
        "name":          device.name,
        "device_type":   device.device_type,
        "hostname":      device.hostname,
        "ip_address":    device.ip_address,
        "capabilities":  device.capabilities,
        "registered_at": device.registered_at,
        "last_seen":     device.last_seen,
        "is_online":     device.is_online,
        "room":          device.room,
    })))
}

#[derive(serde::Deserialize)]
struct RegisterPushTokenRequest {
    token: String,
    /// "fcm" | "apns" | "expo".
    platform: String,
}

/// Upper bound on a stored push token. Real FCM/APNs/Expo tokens are well under
/// 1 KB; the cap stops a client from persisting arbitrarily large blobs.
const MAX_PUSH_TOKEN_LEN: usize = 4096;

/// `POST /api/v1/devices/{id}/push-token` — a paired device registers its
/// current push token (#95). Validates the device exists and the platform is
/// known; replaces any prior token for that device.
async fn register_push_token(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<RegisterPushTokenRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(repo) = state.push_token_repo.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "push-token storage not available" })),
        ));
    };

    let platform = pond_core::user_data::domain::push_token::PushPlatform::parse(&req.platform)
        .ok_or((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid `platform`; use fcm|apns|expo" })),
        ))?;
    if req.token.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "`token` must not be empty" })),
        ));
    }
    if req.token.len() > MAX_PUSH_TOKEN_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "`token` too long" })),
        ));
    }

    // The token must belong to a known, registered device.
    let exists = state.device_registry.get_device(&id).await.map_err(|e| {
        tracing::warn!(error = %e, "push-token: device lookup failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "device lookup failed" })),
        )
    })?;
    if exists.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown device" })),
        ));
    }

    let token = pond_core::user_data::domain::push_token::PushToken {
        device_id: id,
        token: req.token,
        platform,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    repo.upsert(token).await.map_err(|e| {
        tracing::warn!(error = %e, "push-token: upsert failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "could not store push token" })),
        )
    })?;

    Ok(Json(json!({ "ok": true })))
}

/// `DELETE /api/v1/devices/{id}/push-token` — drop a device's push token
/// (logout / unpair). Idempotent.
async fn delete_push_token(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(repo) = state.push_token_repo.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "push-token storage not available" })),
        ));
    };
    repo.delete(&id).await.map_err(|e| {
        tracing::warn!(error = %e, "push-token: delete failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "could not remove push token" })),
        )
    })?;
    Ok(Json(json!({ "ok": true })))
}

async fn get_settings(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let settings = state.settings_repo.get().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to load settings: {}", e)})),
        )
    })?;
    Ok(Json(serde_json::to_value(settings).unwrap_or(json!({}))))
}

async fn update_settings(
    State(state): State<Arc<AppState>>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(patch) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid settings body: {}", e)})),
        )
    })?;

    // Load current settings so we only overwrite the fields the caller provided.
    let current = state.settings_repo.get().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to load current settings: {}", e)})),
        )
    })?;

    // Reject agent_backend="pond" — backend is quarantined (Q2-05, not production-ready).
    if patch.get("agent_backend").and_then(|v| v.as_str()) == Some("pond") {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "agent_backend \"pond\" is not available — backend is not yet production-ready. Use \"goose\"."
            })),
        ));
    }

    // Merge: serialise current → Value, apply patch fields, deserialise back.
    let mut base =
        serde_json::to_value(&current).unwrap_or(serde_json::Value::Object(Default::default()));
    if let (Some(base_obj), Some(patch_obj)) = (base.as_object_mut(), patch.as_object()) {
        for (k, v) in patch_obj {
            base_obj.insert(k.clone(), v.clone());
        }
    }
    let merged: Settings = serde_json::from_value(base).unwrap_or(current);

    state.settings_repo.update(&merged).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save settings: {}", e)})),
        )
    })?;

    // Hot-reload the ModelRouter whenever any provider/model field changes.
    let provider_keys = [
        "chat_provider",
        "chat_model",
        "tool_model",
        "active_whisper_model",
        "active_tts_model",
    ];
    if let Some(obj) = patch.as_object() {
        if obj.keys().any(|k| provider_keys.contains(&k.as_str())) {
            rebuild_llm_provider(&state, &merged).await;

            // Sync role fields → model_role_assignments (source of truth).
            // This ensures CLI `models list` and `/activate` see the same state
            // as the Settings page write path.
            if let Some(repo) = &state.model_repo {
                let role_map: &[(&str, &str, &str)] = &[
                    ("chat", &merged.chat_provider, &merged.chat_model),
                    ("asr", "", &merged.active_whisper_model),
                    ("tts", "", &merged.active_tts_model),
                ];
                for (role, provider, model_name) in role_map {
                    if model_name.is_empty() {
                        let _ = repo.clear_assignment(role).await;
                        continue;
                    }
                    // Derive category from provider
                    let category = match *provider {
                        "local" | "gguf" => "gguf",
                        "ollama" => "ollama",
                        "asr" | "" if *role == "asr" => "whisper",
                        "tts" | "" if *role == "tts" => "tts_piper",
                        _ => "llamafile",
                    };
                    let model_id = format!("{}/{}", category, model_name);
                    let _ = repo.set_assignment(role, &model_id).await;
                }
            }
        }
    }

    // Return the full merged Settings so the frontend can sync its local state
    // without a second GET request.
    Ok(Json(
        serde_json::to_value(&merged).unwrap_or(json!({ "status": "ok" })),
    ))
}

/// Current conditions + short forecast for the dashboard weather widget.
/// Returns `{"enabled": false}` when no location is configured, rather than
/// an error — the dashboard just keeps showing its placeholder in that case.
async fn get_weather(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(provider) = state.weather_provider.as_ref() else {
        return Ok(Json(json!({ "enabled": false })));
    };

    let current = provider.current().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("Failed to fetch weather: {}", e)})),
        )
    })?;
    let forecast = provider.forecast(4).await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("Failed to fetch forecast: {}", e)})),
        )
    })?;

    let (hi, lo) = forecast
        .days
        .first()
        .map(|d| (d.temp_max_c.round() as i64, d.temp_min_c.round() as i64))
        .unwrap_or((
            current.temperature_c.round() as i64,
            current.temperature_c.round() as i64,
        ));

    let upcoming: Vec<Value> = forecast
        .days
        .iter()
        .skip(1)
        .take(3)
        .map(|d| {
            json!({
                "d": weather_short_weekday(&d.date),
                "i": weather_icon_key(&d.description),
                "t": d.temp_max_c.round() as i64,
            })
        })
        .collect();

    Ok(Json(json!({
        "enabled": true,
        "location_name": current.location_name,
        "temp": current.temperature_c.round() as i64,
        "cond": current.description,
        "icon": weather_icon_key(&current.description),
        "hi": hi,
        "lo": lo,
        "hum": current.humidity_pct,
        "wind": current.wind_speed_kmh.round() as i64,
        "sunrise": current.sunrise,
        "sunset": current.sunset,
        "forecast": upcoming,
    })))
}

/// Maps an Open-Meteo/WMO description to one of the icon keys the dashboard
/// widget knows how to render ("sun" | "cloudSun" | "cloud" | "rain").
fn weather_icon_key(description: &str) -> &'static str {
    let d = description.to_lowercase();
    if d.contains("rain")
        || d.contains("drizzle")
        || d.contains("shower")
        || d.contains("snow")
        || d.contains("thunder")
    {
        "rain"
    } else if d.contains("clear sky") {
        "sun"
    } else if d.contains("clear") || d.contains("partly") {
        "cloudSun"
    } else {
        "cloud"
    }
}

/// Formats a `YYYY-MM-DD` forecast date as a short weekday name (e.g. "Tue").
/// Falls back to the raw date string if parsing fails.
fn weather_short_weekday(date: &str) -> String {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(|d| d.format("%a").to_string())
        .unwrap_or_else(|_| date.to_string())
}

/// Rebuild and hot-swap the ModelRouter using the new settings.
/// Called whenever the user changes any provider/model assignment.
async fn rebuild_llm_provider(state: &Arc<AppState>, settings: &Settings) {
    use pond_adapters_llamafile::LlamafileProvider;
    use pond_adapters_ollama::OllamaProvider;
    #[allow(unused_imports)]
    use pond_core::models::ports::provider::LlmProvider as _;

    let url = &state.llamafile_url;
    let data_dir = state.data_dir.clone();
    let max_tokens = settings.llm_max_tokens;
    let temperature = settings.llm_temperature;

    /// Build one `Arc<dyn LlmProvider>` for a given (provider, model) pair.
    ///
    /// `"local"` → `LocalInferenceLlmAdapter` (compiled in with the
    ///   `local-inference` feature; falls back to llamafile otherwise).
    /// `"ollama"` → `OllamaProvider`.
    /// anything else → `LlamafileProvider`.
    async fn build_one(
        provider: &str,
        model: &str,
        url: &str,
        _data_dir: Option<std::path::PathBuf>,
        max_tokens: u32,
        temperature: f32,
    ) -> Arc<dyn LlmProvider> {
        match provider {
            "ollama" => Arc::new(
                OllamaProvider::new(None, Some(model))
                    .with_max_tokens(max_tokens)
                    .with_temperature(temperature),
            ) as Arc<dyn LlmProvider>,

            #[cfg(feature = "local-inference")]
            "local" | "gguf" => {
                use pond_adapters_local_inference::LocalInferenceLlmAdapter;

                // `new_with_data_dir` handles both raw ".gguf" filenames and
                // HuggingFace "repo:quant" IDs, registering the model in Goose's
                // global registry so LocalInferenceProvider can locate the file.
                let result = match &_data_dir {
                    Some(dir) => LocalInferenceLlmAdapter::new_with_data_dir(model, dir).await,
                    None => LocalInferenceLlmAdapter::new(model).await,
                };
                match result {
                    Ok(adapter) => Arc::new(adapter) as Arc<dyn LlmProvider>,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to build LocalInferenceLlmAdapter for '{}': {}; \
                             falling back to llamafile",
                            model,
                            e
                        );
                        Arc::new(
                            LlamafileProvider::new(Some(url))
                                .with_max_tokens(max_tokens)
                                .with_temperature(temperature),
                        ) as Arc<dyn LlmProvider>
                    }
                }
            }

            _ => Arc::new(
                LlamafileProvider::new(Some(url))
                    .with_max_tokens(max_tokens)
                    .with_temperature(temperature),
            ) as Arc<dyn LlmProvider>,
        }
    }

    let effective_chat_provider = settings.chat_provider.clone();
    let effective_chat_model = settings.chat_model.clone();

    let chat = build_one(
        &effective_chat_provider,
        &effective_chat_model,
        url,
        data_dir.clone(),
        max_tokens,
        temperature,
    )
    .await;
    // If chat uses llamafile, ensure the process is running before
    // the new provider goes live (so the first request doesn't time out).
    if effective_chat_provider == "llamafile" {
        if let Some(manager) = &state.llamafile_manager {
            tracing::info!("llamafile provider selected — ensuring server is running");
            manager
                .ensure_started(Some(effective_chat_model.as_str()))
                .await;
        } else {
            tracing::warn!(
                "llamafile provider selected but no LlamafileManager wired in AppState; \
                 process will not be auto-started"
            );
        }
    }

    println!(
        "[model-switch] hot-reloading LLM provider: {}/{}",
        effective_chat_provider, effective_chat_model
    );
    *state.llm_provider.write().await = Some(chat);
    println!(
        "[model-switch] hot-reload complete: {}/{}",
        effective_chat_provider, effective_chat_model
    );
    tracing::info!(
        "LLM provider hot-reloaded: {}/{}",
        effective_chat_provider,
        effective_chat_model,
    );
}

// ── Model registry handlers ───────────────────────────────────────────────────

/// GET /api/v1/models/capabilities — returns the active model's runtime capabilities.
async fn get_model_capabilities(State(state): State<Arc<AppState>>) -> Json<Value> {
    let caps = state.agent.capabilities();
    Json(serde_json::to_value(caps).unwrap_or_default())
}

/// GET /api/v1/models/active-roles — returns the provider+model currently wired for each role.
///
/// Reads from `model_role_assignments` (source of truth) with a settings KV fallback.
async fn get_active_roles(State(state): State<Arc<AppState>>) -> Json<Value> {
    // Try to read from the persistent join table first
    let assignments: std::collections::HashMap<String, String> = state
        .model_repo
        .as_ref()
        .and_then(|r| {
            // Use try_join in a blocking context — we're inside async so use block_in_place
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(r.list_assignments())
            })
            .ok()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|a| (a.role, a.model_id))
        .collect();

    // Fall back to settings KV hot-cache
    let settings = state.settings_repo.get().await.unwrap_or_default();
    let chat_provider = settings.chat_provider.clone();
    let chat_model = settings.chat_model.clone();

    Json(json!({
        "chat":  {
            "provider": chat_provider,
            "model":    chat_model,
            "model_id": assignments.get("chat"),
        },
        "tool": {
            "model": settings.tool_model,
        },
        "asr": { "model_id": assignments.get("asr") },
        "tts": { "model_id": assignments.get("tts") },
        "embedding": {
            "model_id": assignments.get("embedding"),
            "model": settings.active_embedding_model,
            "provider": settings.embedding_provider,
        },
        "router_name": state.llm_provider.read().await
            .as_ref()
            .map(|p| p.model_name())
            .unwrap_or_else(|| "none".to_string()),
    }))
}

/// GET /api/v1/models/memory-status — returns current LLM memory budget snapshot.
async fn get_memory_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let status = state
        .model_scheduler
        .as_ref()
        .map(|s| s.memory_status())
        .unwrap_or_default();

    Json(json!({
        "total_mb":             status.total_mb,
        "available_for_llm_mb": status.available_for_llm_mb,
        "loaded_model":         status.loaded_model,
    }))
}

/// Converts a `ModelRecord` to the API response DTO (`ModelStatusEntry`).
///
/// `assignments` is the list of current role assignments; used to determine
/// the `active` flag (true when any role points to this model).
fn record_to_dto(m: &ModelRecord, assignments: &[ModelRoleAssignment]) -> ModelStatusEntry {
    let active = assignments.iter().any(|a| a.model_id == m.id);
    ModelStatusEntry {
        category: m.category.as_str().to_string(),
        name: m.name.clone(),
        description: m.description.clone(),
        size_mb: m.size_mb,
        downloaded: m.downloaded,
        active,
        url: m.url.clone(),
        hf_id: m.hf_id.clone(),
        filename: m.filename.clone(),
        ram_estimate_mb: m.ram_estimate_mb,
        recommended_role: m.recommended_role.clone(),
        asr_language: m.asr_language.clone(),
        asr_size: m.asr_size.clone(),
        tts_engine: m.tts_engine.clone(),
        config_filename: m.config_filename.clone(),
    }
}

/// Scans model directories for files on disk not yet in the catalog,
/// inserts them as custom entries via the model repository, and returns
/// the newly discovered records.
async fn scan_filesystem_extras(
    data_dir: &std::path::Path,
    model_repo: &Arc<dyn pond_core::models::ports::model_repository::ModelRepository + Send + Sync>,
) -> Vec<ModelRecord> {
    let all = model_repo.list_all().await.unwrap_or_default();
    let known_filenames: std::collections::HashSet<String> =
        all.iter().filter_map(|m| m.filename.clone()).collect();

    let data_dir_owned = data_dir.to_path_buf();
    let known = known_filenames;
    let extras_from_disk = tokio::task::spawn_blocking(move || {
        let scan_dir =
            |dir: std::path::PathBuf, category: ModelCategory, exts: &[&str]| -> Vec<ModelRecord> {
                let mut found = vec![];
                let Ok(rd) = std::fs::read_dir(&dir) else {
                    return found;
                };
                for entry in rd.flatten() {
                    let fname = entry.file_name().to_string_lossy().to_string();
                    if !exts.iter().any(|e| fname.ends_with(e)) {
                        continue;
                    }
                    if known.contains(&fname) {
                        continue;
                    }
                    let size_mb = entry.metadata().map(|m| m.len() / 1_048_576).unwrap_or(0);
                    let name = fname
                        .trim_end_matches(".gguf")
                        .trim_end_matches(".llamafile")
                        .trim_end_matches(".onnx")
                        .trim_end_matches(".bin")
                        .to_string();
                    found.push(ModelRecord {
                        id: ModelRecord::id_for(&category, &name),
                        category: category.clone(),
                        name,
                        filename: Some(fname),
                        description: "(detected on disk)".to_string(),
                        size_mb,
                        url: None,
                        hf_id: None,
                        ram_estimate_mb: None,
                        recommended_role: None,
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
                        is_custom: true,
                    });
                }
                found
            };

        let mut extras = vec![];
        extras.extend(scan_dir(
            data_dir_owned.join("models").join("gguf"),
            ModelCategory::Gguf,
            &[".gguf"],
        ));
        extras.extend(scan_dir(
            data_dir_owned.join("models").join("llm"),
            ModelCategory::Llamafile,
            &[".llamafile", ".exe"],
        ));
        extras.extend(scan_dir(
            data_dir_owned.join("models"),
            ModelCategory::Whisper,
            &[".bin"],
        ));
        extras.extend(scan_dir(
            data_dir_owned.join("models").join("tts"),
            ModelCategory::TtsPiper,
            &[".onnx"],
        ));
        extras
    })
    .await
    .unwrap_or_default();

    // Persist newly discovered models to the catalog
    for m in &extras_from_disk {
        let _ = model_repo.upsert(m).await;
    }

    extras_from_disk
}

/// GET /api/v1/models — returns all catalog models with downloaded/active flags.
async fn list_models(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(model_repo) = &state.model_repo else {
        return Ok(Json(
            json!({"whisper": [], "llamafile": [], "tts": [], "gguf": []}),
        ));
    };

    // Discover any files on disk not yet in the catalog
    if let Some(data_dir) = &state.data_dir {
        let _ = scan_filesystem_extras(data_dir, model_repo).await;
    }

    let records = model_repo.list_all().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    let assignments = model_repo.list_assignments().await.unwrap_or_default();

    let mut whisper = vec![];
    let mut llamafile = vec![];
    let mut tts = vec![];
    let mut gguf = vec![];
    let mut ollama = vec![];
    let mut embedding = vec![];

    for m in &records {
        let v = serde_json::to_value(record_to_dto(m, &assignments)).unwrap_or_default();
        match m.category {
            ModelCategory::Whisper => whisper.push(v),
            ModelCategory::Llamafile => llamafile.push(v),
            ModelCategory::TtsPiper | ModelCategory::TtsHttp => tts.push(v),
            ModelCategory::Gguf => gguf.push(v),
            ModelCategory::Ollama => ollama.push(v),
            ModelCategory::Embedding => embedding.push(v),
        }
    }

    Ok(Json(
        json!({"whisper": whisper, "llamafile": llamafile, "tts": tts, "gguf": gguf, "ollama": ollama, "embedding": embedding}),
    ))
}

/// POST /api/v1/models/scan — explicit filesystem scan, persists and returns newly discovered entries.
async fn scan_models(State(state): State<Arc<AppState>>) -> Json<Value> {
    let (Some(data_dir), Some(model_repo)) = (&state.data_dir, &state.model_repo) else {
        return Json(json!({"found": 0, "entries": []}));
    };

    let extras = scan_filesystem_extras(data_dir, model_repo).await;
    sync_ollama_models(&state.http_client, model_repo).await;

    let count = extras.len();
    let assignments = model_repo.list_assignments().await.unwrap_or_default();
    let entries: Vec<Value> = extras
        .iter()
        .map(|m| serde_json::to_value(record_to_dto(m, &assignments)).unwrap_or_default())
        .collect();

    Json(json!({"found": count, "entries": entries}))
}

/// Fetch installed Ollama models from the local daemon and upsert them into the model repo.
/// Only models Ollama actually has are registered — `downloaded` is always accurate.
async fn sync_ollama_models(
    client: &reqwest::Client,
    model_repo: &Arc<dyn pond_core::models::ports::model_repository::ModelRepository + Send + Sync>,
) {
    let resp = match client
        .get("http://localhost:11434/api/tags")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return,
    };

    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return,
    };

    let Some(models) = body["models"].as_array() else {
        return;
    };

    for m in models {
        let Some(model_name) = m["name"].as_str() else {
            continue;
        };
        let size_mb = m["size"].as_u64().unwrap_or(0) / (1024 * 1024);
        let model_id = ModelRecord::id_for(&ModelCategory::Ollama, model_name);

        let existing = model_repo.get_by_id(&model_id).await.unwrap_or(None);
        let record = ModelRecord {
            id: model_id,
            category: ModelCategory::Ollama,
            name: model_name.to_string(),
            filename: None,
            description: String::new(),
            size_mb,
            url: None,
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: existing
                .as_ref()
                .and_then(|e| e.recommended_role.clone())
                .or_else(|| Some("chat".to_string())),
            context_length: existing.as_ref().and_then(|e| e.context_length),
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
            is_custom: existing.as_ref().map(|e| e.is_custom).unwrap_or(true),
        };
        let _ = model_repo.upsert(&record).await;
    }
}

/// POST /api/v1/models/registry/refresh — refresh the model catalog from upstream sources.
async fn refresh_model_registry(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(model_repo) = state.model_repo.clone() else {
        return Ok(Json(json!({"status": "no_registry"})));
    };
    let Some(catalog_provider) = state.model_catalog_provider.clone() else {
        return Ok(Json(json!({"status": "no_catalog_provider"})));
    };

    let data_dir = state
        .model_storage_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    // Fetch in the background so we don't block on slow network.
    tokio::spawn(async move {
        match catalog_provider.fetch().await {
            Ok((models, _binaries)) => {
                let count = models.len();
                for mut m in models {
                    // Update downloaded flag from disk.
                    m.downloaded = m
                        .filename
                        .as_ref()
                        .map(|f| match m.category {
                            pond_core::models::domain::model_record::ModelCategory::Whisper => {
                                data_dir.join("models").join(f).exists()
                            }
                            pond_core::models::domain::model_record::ModelCategory::Llamafile => {
                                data_dir.join("models").join("llm").join(f).exists()
                            }
                            pond_core::models::domain::model_record::ModelCategory::Gguf => {
                                data_dir.join("models").join("gguf").join(f).exists()
                            }
                            pond_core::models::domain::model_record::ModelCategory::TtsPiper => {
                                data_dir.join("models").join("tts").join(f).exists()
                            }
                            _ => false,
                        })
                        .unwrap_or(matches!(
                            m.category,
                            pond_core::models::domain::model_record::ModelCategory::TtsHttp
                                | pond_core::models::domain::model_record::ModelCategory::Ollama
                        ));
                    if let Err(e) = model_repo.upsert(&m).await {
                        tracing::warn!("Failed to upsert model '{}': {}", m.id, e);
                    }
                }
                tracing::info!("Model registry refreshed: {} records upserted", count);
            }
            Err(e) => tracing::warn!("Registry refresh failed: {}", e),
        }
    });

    Ok(Json(json!({"status": "refresh_started"})))
}

/// GET /api/v1/models/download/progress — return all active/recent downloads.
///
/// Also evicts entries that finished more than 5 minutes ago to prevent
/// unbounded growth of the in-memory tracker over long server uptimes.
async fn get_download_progress(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mut tracker = state.download_tracker.write().await;
    let now = std::time::Instant::now();
    tracker.retain(|_, e| {
        match e.finished_at {
            Some(t) => now.duration_since(t) < std::time::Duration::from_secs(300),
            None => true, // still in progress — keep
        }
    });
    let entries: Vec<&DownloadEntry> = tracker.values().collect();
    Json(json!({"downloads": entries}))
}

/// POST /api/v1/models/{category}/{name}/download — trigger async model download.
async fn download_model(
    State(state): State<Arc<AppState>>,
    Path((category, name)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(model_repo) = state.model_repo.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "registry not available"})),
        ));
    };
    let Some(data_dir) = state.data_dir.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "data_dir not configured"})),
        ));
    };

    let cat = ModelCategory::from_str(&category).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Unknown category '{}'", category)})),
        )
    })?;
    let model_id = ModelRecord::id_for(&cat, &name);

    let m = model_repo
        .get_by_id(&model_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("Model '{}' not found in '{}'", name, category)})),
            )
        })?;

    if m.downloaded {
        return Ok(Json(json!({"status": "already_downloaded", "name": name})));
    }

    // Embedding models are auto-downloaded by fastembed on first use.
    // No URL fetch needed — just confirm readiness.
    if cat == ModelCategory::Embedding {
        return Ok(Json(
            json!({"status": "ready", "name": name, "note": "Embedding model will download automatically on first use"}),
        ));
    }

    let url = m.url.clone().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "model has no download URL"})),
        )
    })?;
    let filename = m.filename.clone().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "model has no filename"})),
        )
    })?;

    // Determine destination path based on category
    let dest = match cat {
        ModelCategory::Whisper => data_dir.join("models").join(&filename),
        ModelCategory::Llamafile => data_dir.join("models").join("llm").join(&filename),
        ModelCategory::Gguf => data_dir.join("models").join("gguf").join(&filename),
        ModelCategory::TtsPiper | ModelCategory::TtsHttp => {
            data_dir.join("models").join("tts").join(&filename)
        }
        ModelCategory::Ollama => data_dir.join("models").join(&filename),
        ModelCategory::Embedding => data_dir.join("models").join("embedding").join(&filename),
    };

    let tracker = Arc::clone(&state.download_tracker);
    let dl_client = state.http_client.clone();
    let dl_filename = filename.clone();
    let dl_category = category.clone();

    // For TTS models, also download the companion config file (.onnx.json)
    let cfg_url = m.config_url.clone();
    let cfg_filename = m.config_filename.clone();
    let cfg_client = state.http_client.clone();
    let cfg_data_dir = data_dir.clone();
    let dl_data_dir = data_dir.clone();

    tokio::spawn(async move {
        spawn_tracked_download(
            url,
            dest,
            dl_filename,
            dl_category,
            tracker,
            dl_client,
            dl_data_dir,
            async move {
                // Download config file before marking as downloaded
                if let (Some(cu), Some(cf)) = (cfg_url, cfg_filename) {
                    let cfg_dest = cfg_data_dir.join("models").join("tts").join(&cf);
                    if let Some(parent) = cfg_dest.parent() {
                        let _ = tokio::fs::create_dir_all(parent).await;
                    }
                    match cfg_client.get(&cu).send().await {
                        Ok(resp) if resp.status().is_success() => {
                            if let Ok(bytes) = resp.bytes().await {
                                let _ = tokio::fs::write(&cfg_dest, &bytes).await;
                            }
                        }
                        _ => tracing::warn!("Failed to download TTS config file {}", cf),
                    }
                }
                let _ = model_repo.set_downloaded(&model_id, true).await;
            },
        )
        .await;
    });

    Ok(Json(
        json!({"status": "download_started", "name": name, "category": category}),
    ))
}

/// DELETE /api/v1/models/{category}/{name} — delete the model file from disk.
///
/// The catalog record is kept (with `downloaded=false`) so the model can be re-downloaded.
/// Returns 409 if the model is currently assigned to any active role.
async fn delete_model(
    State(state): State<Arc<AppState>>,
    Path((category, name)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let Some(model_repo) = state.model_repo.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "registry not available"})),
        ));
    };

    let cat = ModelCategory::from_str(&category).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Unknown category '{}'", category)})),
        )
    })?;
    let model_id = ModelRecord::id_for(&cat, &name);

    let m = model_repo
        .get_by_id(&model_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("Model '{}' not found in '{}'", name, category)})),
            )
        })?;

    // Block deletion if model is assigned to any active role
    let assignments = model_repo.list_assignments().await.unwrap_or_default();
    if let Some(a) = assignments.iter().find(|a| a.model_id == model_id) {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!("Model is assigned to role '{}'. Deactivate it first.", a.role)
            })),
        ));
    }

    // Delete file from disk (ignore not-found)
    if let (Some(filename), Some(data_dir)) = (&m.filename, &state.data_dir) {
        let path = match cat {
            ModelCategory::Whisper => data_dir.join("models").join(filename),
            ModelCategory::Llamafile => data_dir.join("models").join("llm").join(filename),
            ModelCategory::Gguf => data_dir.join("models").join("gguf").join(filename),
            ModelCategory::TtsPiper | ModelCategory::TtsHttp => {
                data_dir.join("models").join("tts").join(filename)
            }
            ModelCategory::Ollama => data_dir.join("models").join(filename),
            ModelCategory::Embedding => data_dir.join("models").join("embedding").join(filename),
        };
        if path.exists() {
            tokio::fs::remove_file(&path).await.map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("Failed to delete file: {e}")})),
                )
            })?;
        }
        // Also delete companion config file for TTS models (.onnx.json)
        if let Some(cfg_filename) = &m.config_filename {
            let cfg_path = data_dir.join("models").join("tts").join(cfg_filename);
            if cfg_path.exists() {
                let _ = tokio::fs::remove_file(&cfg_path).await;
            }
        }
    }

    model_repo
        .set_downloaded(&model_id, false)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/v1/models/cleanup — sweep unreferenced HF-cache blobs.
///
/// Walks `{data_dir}/hf_cache/hub/models--*/blobs/*`, removes blobs that no
/// flat-path symlink under `{data_dir}/models/**` points at AND whose
/// basename does not appear in `model_role_assignments`. Also clears stale
/// `.incomplete` resume files (> 24h) and fully-broken snapshot dirs.
///
/// Returns `{reclaimed_bytes, removed: [{path, category, bytes}]}`.
async fn cleanup_models(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(data_dir) = state.data_dir.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "data_dir not configured"})),
        ));
    };

    // Protected filenames = every currently-assigned model's `filename`.
    // We never delete a blob whose basename matches an active role.
    let mut protected: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(model_repo) = &state.model_repo {
        if let Ok(assignments) = model_repo.list_assignments().await {
            for a in &assignments {
                if let Ok(Some(m)) = model_repo.get_by_id(&a.model_id).await {
                    if let Some(fname) = m.filename {
                        protected.insert(fname);
                    }
                    // Also protect by the bare model name — covers blobs whose
                    // basename matches `{name}` (e.g. legacy migrations).
                    protected.insert(m.name.clone());
                    if let Some(cfg) = m.config_filename {
                        protected.insert(cfg);
                    }
                }
            }
        }
    }

    let report = crate::cleanup::run_cleanup(&data_dir, &protected)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;

    Ok(Json(serde_json::to_value(&report).unwrap_or_else(
        |_| json!({"reclaimed_bytes": 0, "removed": []}),
    )))
}

/// GET /api/v1/models/disk-usage — per-category bytes plus HF cache totals.
///
/// Walks `{data_dir}/models/**` and `{data_dir}/hf_cache/**`, returning
/// `{total_bytes, by_category, hf_cache_bytes, incomplete_bytes}`. The flat
/// paths under `models/<cat>` resolve through symlinks so the same blob is
/// counted once per category bucket, never duplicated.
async fn disk_usage(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(data_dir) = state.data_dir.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "data_dir not configured"})),
        ));
    };

    let usage = crate::cleanup::collect_disk_usage(&data_dir)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;

    Ok(Json(serde_json::to_value(&usage).unwrap_or_else(|_| {
        json!({"total_bytes": 0, "by_category": {}, "hf_cache_bytes": 0, "incomplete_bytes": 0})
    })))
}

/// POST /api/v1/models/{category}/{name}/activate — assign model to a role.
///
/// Body: `{ "role": "chat" | "think" | "task" | "asr" | "tts" }`
///
/// Also syncs to the settings KV hot-cache and rebuilds `ModelRouter` for LLM roles.
async fn activate_model(
    State(state): State<Arc<AppState>>,
    Path((category, name)): Path<(String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(model_repo) = state.model_repo.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "registry not available"})),
        ));
    };

    let Json(body) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid JSON: {e}")})),
        )
    })?;
    let role = body["role"].as_str().unwrap_or("").to_string();
    if role.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "role is required"})),
        ));
    }

    let cat = ModelCategory::from_str(&category).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Unknown category '{}'", category)})),
        )
    })?;

    // Validate role ↔ category compatibility
    if !ModelRoleAssignment::category_matches_role(&cat, &role) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!(
                    "Category '{}' cannot be assigned to role '{}'. \
                     LLM roles (chat/think/task) require gguf/llamafile/ollama; \
                     asr requires whisper; tts requires tts_piper/tts_http.",
                    category, role
                )
            })),
        ));
    }

    let model_id = ModelRecord::id_for(&cat, &name);
    let record = model_repo
        .get_by_id(&model_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("Model '{}' not found in '{}'", name, category)})),
            )
        })?;

    // Persist provider keys using runtime provider names (not category names).
    // GGUF category maps to the "local" provider in runtime routing.
    let provider = match cat {
        ModelCategory::Gguf => "local",
        ModelCategory::Llamafile => "llamafile",
        ModelCategory::Ollama => "ollama",
        ModelCategory::Whisper => "asr",
        ModelCategory::TtsPiper => "tts",
        ModelCategory::TtsHttp => "tts",
        ModelCategory::Embedding => "embedding",
    };

    // Persist assignment
    model_repo
        .set_assignment(&role, &model_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;

    // Sync to settings KV hot-cache
    let settings_repo = state.settings_repo.clone();
    match role.as_str() {
        "chat" => {
            let _ = settings_repo.set_key("chat_model", name.clone()).await;
            let _ = settings_repo
                .set_key("chat_provider", provider.to_string())
                .await;
        }
        "tool" => {
            let _ = settings_repo.set_key("tool_model", name.clone()).await;
        }
        "asr" => {
            let _ = settings_repo
                .set_key("active_whisper_model", name.clone())
                .await;
        }
        "tts" => {
            let _ = settings_repo
                .set_key("active_tts_model", name.clone())
                .await;
        }
        "embedding" => {
            let _ = settings_repo
                .set_key("active_embedding_model", name.clone())
                .await;
            let _ = settings_repo
                .set_key("embedding_provider", provider.to_string())
                .await;
        }
        _ => {}
    }

    // Hot-rebuild the ModelRouter for LLM roles using the existing helper
    if matches!(role.as_str(), "chat" | "think" | "task") {
        // Memory-fit guard (Phase 6): warn if the model will not fully reside in
        // the device LLM budget and therefore spill to CPU (single-digit tok/s).
        //
        // This is cross-platform-safe: it only *logs*. On Jetson the recommended
        // fail-closed behavior (drop_caches + -ngl residency check, and refusing
        // a spilling full-GPU load) belongs in the local-inference loader and is
        // NOT done here — see scripts/jetson-llama-optimization and the note in
        // .ai/scratchpad.md. We do not touch the loader from the Mac build.
        warn_if_model_spills(&state, &record).await;

        let settings = state.settings_repo.get().await.unwrap_or_default();
        rebuild_llm_provider(&state, &settings).await;
    }

    Ok(Json(json!({"role": role, "model_id": model_id})))
}

/// Headroom (MB) reserved on top of a model's own weights for KV cache + system
/// slack. Mirrors `DEFAULT_HEADROOM_MB` in the desktop `modelFit` helper so the
/// server-side warning and the UI verdict agree.
const MEMORY_FIT_HEADROOM_MB: u64 = 1024;

/// Pure fit decision: does a model of `residency_mb` spill on a device with
/// `available_for_llm_mb` free, reserving `MEMORY_FIT_HEADROOM_MB` headroom?
///
/// Returns `None` when there is no basis for a verdict — the budget is absent
/// (`available_for_llm_mb == 0`, e.g. NoopScheduler / Mac dev) or the model size
/// is unknown (`residency_mb == 0`). Returns `Some(true)` when the model spills,
/// `Some(false)` when it fits. Mirrors the desktop `modelFit` helper.
fn model_spills_budget(residency_mb: u64, available_for_llm_mb: u64) -> Option<bool> {
    if available_for_llm_mb == 0 || residency_mb == 0 {
        return None;
    }
    let budget = available_for_llm_mb.saturating_sub(MEMORY_FIT_HEADROOM_MB);
    Some(residency_mb > budget)
}

/// Logs a warning when a model being activated for an LLM role is larger than
/// the device's LLM memory budget (minus headroom) and will therefore spill to
/// CPU and run slowly.
///
/// Cross-platform-safe: this only *logs*. When the scheduler reports no budget
/// (`total_mb == 0`, e.g. NoopScheduler for llamafile/ollama or a Mac dev
/// machine) it stays silent — there is nothing to compare against. The residency
/// estimate prefers `size_mb` (on-disk weights) and falls back to
/// `ram_estimate_mb`.
async fn warn_if_model_spills(state: &Arc<AppState>, record: &ModelRecord) {
    let Some(scheduler) = state.model_scheduler.as_ref() else {
        return;
    };
    let status = scheduler.memory_status();
    if status.total_mb == 0 {
        return;
    }

    let residency_mb = if record.size_mb > 0 {
        record.size_mb
    } else {
        record.ram_estimate_mb.unwrap_or(0)
    };

    if model_spills_budget(residency_mb, status.available_for_llm_mb) == Some(true) {
        let budget = status
            .available_for_llm_mb
            .saturating_sub(MEMORY_FIT_HEADROOM_MB);
        tracing::warn!(
            model = %record.name,
            model_size_mb = residency_mb,
            available_for_llm_mb = status.available_for_llm_mb,
            budget_mb = budget,
            "model exceeds device LLM memory budget — it will spill to CPU and \
             run slowly. On Jetson, enable the fail-closed loader path \
             (drop_caches + -ngl residency check) or pick a model that fits the \
             GPU budget. See scripts/jetson-llama-optimization."
        );
    }
}

/// GET /api/v1/models/ollama — proxy Ollama's /api/tags to list available local models.
/// Returns `{"models": [...]}` or `{"models": [], "error": "..."}` if Ollama is unreachable.
async fn list_ollama_models(State(state): State<Arc<AppState>>) -> Json<Value> {
    let client = &state.http_client;
    match client
        .get("http://localhost:11434/api/tags")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let body: Value = resp.json().await.unwrap_or(json!({"models": []}));
            Json(body)
        }
        Ok(resp) => {
            Json(json!({"models": [], "error": format!("Ollama returned {}", resp.status())}))
        }
        Err(_) => Json(json!({"models": [], "error": "Ollama not running or not installed"})),
    }
}

/// POST /api/v1/models/ollama/pull — trigger `ollama pull <model>` on the server.
async fn pull_ollama_model(body: Result<Json<Value>, JsonRejection>) -> (StatusCode, Json<Value>) {
    let model = match body {
        Ok(Json(v)) => v["model"].as_str().unwrap_or("").to_string(),
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "expected {\"model\":\"name\"}"})),
            )
        }
    };
    if model.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "model name is required"})),
        );
    }
    // Spawn `ollama pull <model>` as a background process (non-blocking).
    match tokio::process::Command::new("ollama")
        .args(["pull", &model])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => (
            StatusCode::ACCEPTED,
            Json(json!({"status": "pulling", "model": model})),
        ),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": format!("ollama not found: {e}")})),
        ),
    }
}

/// GET /api/v1/models/search/gguf?q=<query> — proxy HuggingFace API for GGUF models.
async fn search_gguf_models(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let q = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let url = format!(
        "https://huggingface.co/api/models?filter=gguf&search={}&limit=20&sort=downloads&direction=-1",
        urlencoding::encode(q)
    );
    let client = &state.http_client;
    match client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .header(
            "user-agent",
            concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let models: Vec<Value> = resp.json().await.unwrap_or_default();
            // Return a simplified shape: id, downloads, likes, tags
            let simplified: Vec<Value> = models.into_iter().map(|m| json!({
                "id":        m["id"],
                "downloads": m["downloads"],
                "likes":     m["likes"],
                "tags":      m["tags"],
                "url":       format!("https://huggingface.co/{}", m["id"].as_str().unwrap_or("")),
            })).collect();
            Json(json!({"models": simplified}))
        }
        Ok(resp) => {
            Json(json!({"models": [], "error": format!("HuggingFace returned {}", resp.status())}))
        }
        Err(e) => Json(json!({"models": [], "error": format!("Request failed: {e}")})),
    }
}

/// GET /api/v1/models/search/llamafile?q=<query> — list llamafile releases from GitHub.
async fn search_llamafile_models(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let q = params
        .get("q")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let url = "https://api.github.com/repos/Mozilla-Ocho/llamafile/releases?per_page=5";
    let client = &state.http_client;
    match client
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .header(
            "user-agent",
            concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let releases: Vec<Value> = resp.json().await.unwrap_or_default();
            let mut assets: Vec<Value> = Vec::new();
            for release in &releases {
                let tag = release["tag_name"].as_str().unwrap_or("");
                if let Some(arr) = release["assets"].as_array() {
                    for asset in arr {
                        let name = asset["name"].as_str().unwrap_or("");
                        // Only include .llamafile executables, filter by query
                        if name.ends_with(".llamafile") || name.ends_with(".llamafile.exe") {
                            if q.is_empty() || name.to_lowercase().contains(&q) {
                                let size_mb = asset["size"].as_u64().unwrap_or(0) / (1024 * 1024);
                                assets.push(json!({
                                    "name":       name,
                                    "version":    tag,
                                    "size_mb":    size_mb,
                                    "url":        asset["browser_download_url"],
                                    "release_url": release["html_url"],
                                }));
                            }
                        }
                    }
                }
            }
            Json(json!({"models": assets}))
        }
        Ok(resp) => {
            Json(json!({"models": [], "error": format!("GitHub returned {}", resp.status())}))
        }
        Err(e) => Json(json!({"models": [], "error": format!("Request failed: {e}")})),
    }
}

/// GET /api/v1/models/search/gguf/files?repo=<owner/name> — list .gguf files inside a HF repo.
async fn list_hf_model_files(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let repo = match params.get("repo") {
        Some(r) if !r.is_empty() => r.clone(),
        _ => return Json(json!({"files": [], "error": "repo param required"})),
    };
    // Do NOT percent-encode the repo — HF expects the literal owner/name path segment
    // (urlencoding::encode would turn '/' into '%2F' which returns 400)
    let url = format!("https://huggingface.co/api/models/{}", repo);
    let client = &state.http_client;
    match client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .header(
            "user-agent",
            concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let meta: Value = resp.json().await.unwrap_or_default();
            let files: Vec<Value> = meta["siblings"]
                .as_array()
                .map(|siblings| {
                    siblings.iter()
                        .filter(|s| {
                            s["rfilename"].as_str()
                                .map(|n| n.ends_with(".gguf"))
                                .unwrap_or(false)
                        })
                        .map(|s| {
                            let filename = s["rfilename"].as_str().unwrap_or("").to_string();
                            let size_mb  = s["size"].as_u64().map(|b| b / 1_048_576);
                            json!({
                                "filename": filename,
                                "size_mb":  size_mb,
                                "url": format!("https://huggingface.co/{}/resolve/main/{}", repo, filename),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Json(json!({"files": files}))
        }
        Ok(resp) => {
            Json(json!({"files": [], "error": format!("HuggingFace returned {}", resp.status())}))
        }
        Err(e) => Json(json!({"files": [], "error": format!("Request failed: {e}")})),
    }
}

/// POST /api/v1/models/download/url — download a model file by URL into the right folder.
/// Body: { "url": "https://...", "category": "gguf"|"llamafile"|"whisper"|"tts", "filename": "model.gguf" }
async fn download_model_from_url(
    State(state): State<Arc<AppState>>,
    body: Result<Json<Value>, JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let Json(body) = match body {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid JSON body"})),
            )
        }
    };
    let url = body["url"].as_str().unwrap_or("").to_string();
    let category = body["category"].as_str().unwrap_or("gguf").to_string();
    let filename = body["filename"].as_str().unwrap_or("").to_string();

    if url.is_empty() || filename.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "url and filename are required"})),
        );
    }
    if !url.starts_with("https://") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "only https URLs are accepted"})),
        );
    }

    let Some(data_dir) = state.data_dir.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "data_dir not configured"})),
        );
    };

    let dest = match category.as_str() {
        "whisper" => data_dir.join("models").join(&filename),
        "llamafile" => data_dir.join("models").join("llm").join(&filename),
        "gguf" => data_dir.join("models").join("gguf").join(&filename),
        "tts" => data_dir.join("models").join("tts").join(&filename),
        _ => data_dir.join("models").join(&filename),
    };

    let tracker = Arc::clone(&state.download_tracker);
    let resp_filename = filename.clone();
    let resp_category = category.clone();

    let dl_client = state.http_client.clone();
    let dl_data_dir = data_dir.clone();
    tokio::spawn(async move {
        spawn_tracked_download(
            url,
            dest,
            filename,
            category,
            tracker,
            dl_client,
            dl_data_dir,
            async {},
        )
        .await;
    });

    (
        StatusCode::ACCEPTED,
        Json(
            json!({"status": "downloading", "filename": resp_filename, "category": resp_category}),
        ),
    )
}

/// Shared streaming download with progress tracking.
/// Streams the URL to `dest`, updating `tracker` as each chunk arrives.
/// HF URLs route through `pond_hf_cache` for resumable + etag-aware fetches with
/// auth surviving HF→CDN redirects. Non-HF URLs use the existing reqwest path.
/// Calls `on_done` (an async closure) when the download completes successfully.
async fn spawn_tracked_download<F>(
    url: String,
    dest: std::path::PathBuf,
    filename: String,
    category: String,
    tracker: Arc<tokio::sync::RwLock<std::collections::HashMap<String, DownloadEntry>>>,
    client: reqwest::Client,
    data_dir: std::path::PathBuf,
    on_done: F,
) where
    F: std::future::Future<Output = ()> + Send,
{
    use tokio::io::AsyncWriteExt;

    // Register as in-progress
    {
        let mut t = tracker.write().await;
        t.insert(
            filename.clone(),
            DownloadEntry {
                filename: filename.clone(),
                category: category.clone(),
                downloaded_bytes: 0,
                total_bytes: None,
                status: "downloading".to_string(),
                finished_at: None,
            },
        );
    }

    if let Some(parent) = dest.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    tracing::info!("Downloading {} from {}", filename, url);

    let result: Result<(), String> =
        if let Some((repo_id, revision, fname)) = pond_hf_cache::parse_hf_url(&url) {
            download_via_hf_cache_tracked(
                &repo_id, &revision, &fname, &dest, &data_dir, &filename, &tracker,
            )
            .await
        } else {
            async {
                let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
                if !resp.status().is_success() {
                    return Err(format!("HTTP {}", resp.status()));
                }

                let total = resp.content_length();
                {
                    let mut t = tracker.write().await;
                    if let Some(e) = t.get_mut(&filename) {
                        e.total_bytes = total;
                    }
                }

                let mut file = tokio::fs::File::create(&dest)
                    .await
                    .map_err(|e| e.to_string())?;

                let mut downloaded: u64 = 0;
                let mut resp = resp;
                while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
                    file.write_all(&chunk).await.map_err(|e| e.to_string())?;
                    downloaded += chunk.len() as u64;
                    let mut t = tracker.write().await;
                    if let Some(e) = t.get_mut(&filename) {
                        e.downloaded_bytes = downloaded;
                    }
                }
                file.flush().await.map_err(|e| e.to_string())?;
                Ok(())
            }
            .await
        };

    match result {
        Ok(()) => {
            tracing::info!("Downloaded {} to {:?}", filename, dest);
            {
                let mut t = tracker.write().await;
                if let Some(e) = t.get_mut(&filename) {
                    e.status = "done".to_string();
                    e.finished_at = Some(std::time::Instant::now());
                }
            }
            on_done.await;
        }
        Err(err) => {
            tracing::error!("Download {} failed: {}", filename, err);
            let mut t = tracker.write().await;
            if let Some(e) = t.get_mut(&filename) {
                e.status = "error".to_string();
                e.finished_at = Some(std::time::Instant::now());
            }
        }
    }
}

/// HF URL → hardened resumable fetch, with progress mirrored to the same
/// `DownloadEntry` tracker that the legacy reqwest path updates. After the blob
/// lands in `hf_cache/blobs/{etag}`, we symlink `dest` to it so existing
/// filesystem lookups keep returning the same flat path.
async fn download_via_hf_cache_tracked(
    repo_id: &str,
    revision: &str,
    fname: &str,
    dest: &std::path::Path,
    data_dir: &std::path::Path,
    tracker_key: &str,
    tracker: &Arc<tokio::sync::RwLock<std::collections::HashMap<String, DownloadEntry>>>,
) -> Result<(), String> {
    let cache = pond_hf_cache::HfCache::new(data_dir);
    let token: Option<String> = hf_token_from_env().or_else(|| cache.token().map(String::from));
    let client =
        pond_hf_cache::build_redirect_aware_client(token.as_deref()).map_err(|e| e.to_string())?;

    let repo = cache
        .repo(repo_id.to_string())
        .with_revision(revision.to_string());
    let fetch = repo.file(fname.to_string());

    let tracker_owned = Arc::clone(tracker);
    let tracker_key_owned = tracker_key.to_string();
    let progress = move |downloaded: u64, total: u64| {
        let tracker_owned = Arc::clone(&tracker_owned);
        let key = tracker_key_owned.clone();
        tokio::spawn(async move {
            let mut t = tracker_owned.write().await;
            if let Some(e) = t.get_mut(&key) {
                e.downloaded_bytes = downloaded;
                if total > 0 && e.total_bytes != Some(total) {
                    e.total_bytes = Some(total);
                }
            }
        });
    };

    let blob_path = fetch
        .download_to_blob(&client, token.as_deref(), progress)
        .await
        .map_err(|e| e.to_string())?;

    if let Some(parent) = dest.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let _ = tokio::fs::remove_file(dest).await;
    link_or_copy_blob(&blob_path, dest)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn hf_token_from_env() -> Option<String> {
    for var in ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN", "HUGGINGFACE_TOKEN"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(unix)]
async fn link_or_copy_blob(src: &std::path::Path, dest: &std::path::Path) -> anyhow::Result<()> {
    let src = src.to_path_buf();
    let dest = dest.to_path_buf();
    let label = format!("symlink {} -> {}", dest.display(), src.display());
    tokio::task::spawn_blocking(move || std::os::unix::fs::symlink(&src, &dest))
        .await
        .map_err(|e| anyhow::anyhow!("symlink task panicked: {e}"))?
        .map_err(|e| anyhow::anyhow!("{label}: {e}"))
}

#[cfg(not(unix))]
async fn link_or_copy_blob(src: &std::path::Path, dest: &std::path::Path) -> anyhow::Result<()> {
    tokio::fs::copy(src, dest)
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("copy {} -> {}: {e}", dest.display(), src.display()))
}

// ── Profile handlers ──────────────────────────────────────────────────────────

async fn list_profiles(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let profiles = state.profile_repo.list().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    let list: Vec<Value> = profiles
        .iter()
        .map(|p| {
            json!({
                "id":           p.id,
                "display_name": p.display_name,
                "avatar_emoji": p.avatar_emoji,
                "preferences":  p.preferences,
                "created_at":   p.created_at.to_rfc3339(),
                "updated_at":   p.updated_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(json!({ "profiles": list })))
}

async fn create_profile(
    State(state): State<Arc<AppState>>,
    body: Result<Json<CreateProfileRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;
    let profile = state.profile_repo.create(req).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id":           profile.id,
            "display_name": profile.display_name,
            "avatar_emoji": profile.avatar_emoji,
            "preferences":  profile.preferences,
            "created_at":   profile.created_at.to_rfc3339(),
            "updated_at":   profile.updated_at.to_rfc3339(),
        })),
    ))
}

async fn get_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let profile = state.profile_repo.get(&id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    match profile {
        Some(p) => Ok(Json(json!({
            "id":           p.id,
            "display_name": p.display_name,
            "avatar_emoji": p.avatar_emoji,
            "preferences":  p.preferences,
            "created_at":   p.created_at.to_rfc3339(),
            "updated_at":   p.updated_at.to_rfc3339(),
        }))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "profile not found"})),
        )),
    }
}

#[derive(serde::Deserialize)]
struct UpdatePrefsRequest {
    preferences: std::collections::HashMap<String, String>,
}

async fn update_profile_prefs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<UpdatePrefsRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;
    let profile = state
        .profile_repo
        .update_preferences(&id, req.preferences)
        .await
        .map_err(|e| {
            let status = if e.to_string().contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(json!({"error": e.to_string()})))
        })?;
    Ok(Json(json!({
        "id":           profile.id,
        "display_name": profile.display_name,
        "preferences":  profile.preferences,
        "updated_at":   profile.updated_at.to_rfc3339(),
    })))
}

async fn delete_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    state.profile_repo.delete(&id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Sensor handlers ───────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct SensorReadingRequest {
    device_id: String,
    sensor_type: String,
    value: f64,
    unit: String,
}

async fn record_sensor(
    State(state): State<Arc<AppState>>,
    body: Result<Json<SensorReadingRequest>, JsonRejection>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;
    let reading = SensorReading {
        device_id: req.device_id,
        sensor_type: req.sensor_type,
        value: req.value,
        unit: req.unit,
        recorded_at: chrono::Utc::now(),
    };
    state
        .sensor_storage
        .record(reading.clone())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;
    // Publish to the in-process bus only after the write succeeds (#91), so
    // reactive consumers never see an event for a reading that failed to persist.
    if let Some(bus) = &state.event_bus {
        bus.publish(BusEvent::Sensor(reading));
    }
    Ok(StatusCode::CREATED)
}

#[derive(serde::Deserialize)]
struct SensorQueryParams {
    limit: Option<usize>,
    /// Filter by sensor type (e.g. "temperature", "humidity").
    sensor_type: Option<String>,
    /// RFC3339 inclusive start time for history queries.
    since: Option<String>,
    /// RFC3339 exclusive end time for history queries.
    until: Option<String>,
    /// Aggregation function: "min", "max", "avg", or "current". Omit for raw history.
    agg: Option<String>,
}

async fn get_recent_sensors(
    State(state): State<Arc<AppState>>,
    Path(device_id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<SensorQueryParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let agg = params.agg.as_deref().map(str::to_lowercase);

    // When a sensor_type + time range is given, use the history query path.
    if let Some(ref sensor_type) = params.sensor_type {
        if agg.as_deref() == Some("current")
            || (params.since.is_none() && params.until.is_none() && agg.is_none())
        {
            // Fall through to latest-value query below only when no time bounds.
        } else {
            let since = params.since.as_deref().and_then(parse_sensor_datetime);
            let until = params.until.as_deref().and_then(parse_sensor_datetime);
            let readings = state
                .sensor_storage
                .get_history(&device_id, sensor_type, since, until)
                .await
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": e.to_string()})),
                    )
                })?;

            return match agg.as_deref() {
                Some("min") => {
                    let val = readings
                        .iter()
                        .map(|r| r.value)
                        .fold(f64::INFINITY, f64::min);
                    Ok(Json(
                        json!({ "device_id": device_id, "sensor_type": sensor_type, "min": val, "count": readings.len() }),
                    ))
                }
                Some("max") => {
                    let val = readings
                        .iter()
                        .map(|r| r.value)
                        .fold(f64::NEG_INFINITY, f64::max);
                    Ok(Json(
                        json!({ "device_id": device_id, "sensor_type": sensor_type, "max": val, "count": readings.len() }),
                    ))
                }
                Some("avg") => {
                    let avg = if readings.is_empty() {
                        serde_json::Value::Null
                    } else {
                        let sum: f64 = readings.iter().map(|r| r.value).sum();
                        serde_json::Value::from(sum / readings.len() as f64)
                    };
                    Ok(Json(
                        json!({ "device_id": device_id, "sensor_type": sensor_type, "avg": avg, "count": readings.len() }),
                    ))
                }
                _ => {
                    let list: Vec<Value> = readings
                        .iter()
                        .map(|r| {
                            json!({
                                "device_id":   r.device_id,
                                "sensor_type": r.sensor_type,
                                "value":       r.value,
                                "unit":        r.unit,
                                "recorded_at": r.recorded_at.to_rfc3339(),
                            })
                        })
                        .collect();
                    Ok(Json(json!({ "readings": list })))
                }
            };
        }
    }

    // Current-value query: latest reading per sensor_type (or all types).
    if let Some(ref sensor_type) = params.sensor_type {
        if agg.as_deref() == Some("current") || params.since.is_none() {
            let reading = state
                .sensor_storage
                .get_latest(&device_id, sensor_type)
                .await
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": e.to_string()})),
                    )
                })?;
            return match reading {
                Some(r) => Ok(Json(json!({
                    "device_id":   r.device_id,
                    "sensor_type": r.sensor_type,
                    "value":       r.value,
                    "unit":        r.unit,
                    "recorded_at": r.recorded_at.to_rfc3339(),
                }))),
                None => Ok(Json(json!({ "readings": [] }))),
            };
        }
    }

    // Default: recent readings (all types) with a limit.
    let limit = params.limit.unwrap_or(20).min(100);
    let readings = state
        .sensor_storage
        .get_recent(&device_id, limit)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;
    let list: Vec<Value> = readings
        .iter()
        .map(|r| {
            json!({
                "device_id":   r.device_id,
                "sensor_type": r.sensor_type,
                "value":       r.value,
                "unit":        r.unit,
                "recorded_at": r.recorded_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(json!({ "readings": list })))
}

fn parse_sensor_datetime(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    s.parse::<chrono::DateTime<chrono::Utc>>().ok().or_else(|| {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
            .ok()
            .map(|ndt| ndt.and_utc())
    })
}

// ── Activity query API (#114) ──────────────────────────────────────────────────

/// Upper bound on rows returned by the activity endpoints, regardless of the
/// requested `limit`, so a single query can't pull unbounded data into memory.
const ACTIVITY_MAX_LIMIT: usize = 1000;

#[derive(serde::Deserialize)]
struct ActivityQueryParams {
    /// RFC3339 inclusive lower bound on timestamp.
    since: Option<String>,
    /// RFC3339 exclusive upper bound on timestamp.
    until: Option<String>,
    /// `EventCategory` in snake_case (e.g. "sensor", "device", "auth").
    category: Option<String>,
    session_id: Option<String>,
    limit: Option<usize>,
}

/// Parse an `EventCategory` from its snake_case wire form.
fn parse_event_category(s: &str) -> Option<EventCategory> {
    serde_json::from_value(Value::String(s.to_string())).ok()
}

/// Parse an optional RFC3339 timestamp query param, erroring on malformed input.
fn parse_rfc3339_param(
    field: &str,
    raw: Option<String>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, (StatusCode, Json<Value>)> {
    match raw {
        None => Ok(None),
        Some(s) => chrono::DateTime::parse_from_rfc3339(&s)
            .map(|dt| Some(dt.with_timezone(&chrono::Utc)))
            .map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("invalid `{field}`: expected an RFC3339 timestamp") })),
                )
            }),
    }
}

/// `GET /api/v1/activity` — recent events, newest first, with optional
/// `since` / `until` / `category` / `session_id` / `limit` filters.
///
/// Secret-classified events are never returned: the store query excludes them
/// (`max_sensitivity`), so `limit` counts only surfaceable events, and the
/// handler re-filters as defense in depth (such events should not be logged
/// at all, but the API also refuses to surface them).
async fn get_activity(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ActivityQueryParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(event_log) = state.event_log.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "event log not available" })),
        ));
    };

    let category = match params.category.as_deref() {
        Some(c) => Some(parse_event_category(c).ok_or((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid `category`" })),
        ))?),
        None => None,
    };

    let query = EventQuery {
        category,
        session_id: params.session_id,
        trace_id: None,
        since: parse_rfc3339_param("since", params.since)?,
        until: parse_rfc3339_param("until", params.until)?,
        min_sensitivity: None,
        max_sensitivity: Some(PrivacySensitivity::Sensitive),
        limit: Some(params.limit.unwrap_or(100).min(ACTIVITY_MAX_LIMIT)),
    };

    let events = event_log.query(query).await.map_err(|e| {
        tracing::warn!(error = %e, "activity query failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "activity query failed" })),
        )
    })?;

    let visible: Vec<Value> = events
        .into_iter()
        .filter(|e| e.privacy_sensitivity != PrivacySensitivity::Secret)
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect();

    Ok(Json(json!({ "count": visible.len(), "events": visible })))
}

/// `DELETE /api/v1/activity` — the user "clear my activity" control (#117).
/// With no query params it purges the entire event log; `category` / `since` /
/// `until` / `session_id` narrow the purge. Returns the number of events removed.
async fn clear_activity(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ActivityQueryParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(event_log) = state.event_log.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "event log not available" })),
        ));
    };

    let category = match params.category.as_deref() {
        Some(c) => Some(parse_event_category(c).ok_or((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid `category`" })),
        ))?),
        None => None,
    };

    let query = EventQuery {
        category,
        session_id: params.session_id,
        trace_id: None,
        since: parse_rfc3339_param("since", params.since)?,
        until: parse_rfc3339_param("until", params.until)?,
        min_sensitivity: None,
        // No max: "clear my activity" must be able to purge Secret rows too.
        max_sensitivity: None,
        limit: None,
    };

    let purged = event_log.purge(query).await.map_err(|e| {
        tracing::warn!(error = %e, "activity purge failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "activity purge failed" })),
        )
    })?;

    Ok(Json(json!({ "purged": purged })))
}

#[derive(serde::Deserialize)]
struct ActivitySummaryParams {
    /// Time window: "hour" | "day" (default) | "week".
    window: Option<String>,
}

/// `GET /api/v1/activity/summary` — "what happened in the last hour/day/week":
/// total count + per-category breakdown over the window (excluding Secret
/// events). Aggregated over up to `ACTIVITY_MAX_LIMIT` recent in-window events.
async fn activity_summary(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ActivitySummaryParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(event_log) = state.event_log.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "event log not available" })),
        ));
    };

    let window = params.window.as_deref().unwrap_or("day");
    let span = match window {
        "hour" => chrono::Duration::hours(1),
        "day" => chrono::Duration::days(1),
        "week" => chrono::Duration::weeks(1),
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("invalid `window` {other:?}; use hour|day|week") })),
            ))
        }
    };
    let since = chrono::Utc::now() - span;

    let events = event_log
        .query(EventQuery {
            since: Some(since),
            max_sensitivity: Some(PrivacySensitivity::Sensitive),
            limit: Some(ACTIVITY_MAX_LIMIT),
            ..Default::default()
        })
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "activity summary query failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "activity summary query failed" })),
            )
        })?;

    let mut by_category: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut total = 0usize;
    for event in events
        .iter()
        .filter(|e| e.privacy_sensitivity != PrivacySensitivity::Secret)
    {
        let key = serde_json::to_value(event.category)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        *by_category.entry(key).or_default() += 1;
        total += 1;
    }

    Ok(Json(json!({
        "window": window,
        "since": since.to_rfc3339(),
        "total": total,
        "by_category": by_category,
    })))
}

// ── Camera handlers ───────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct CameraEventRequest {
    camera_id: String,
    event_type: String,
    confidence: Option<f64>,
    snapshot_path: Option<String>,
    metadata: Option<String>,
}

#[derive(serde::Deserialize)]
struct CameraQueryParams {
    camera_id: Option<String>,
    limit: Option<usize>,
}

async fn record_camera_event(
    State(state): State<Arc<AppState>>,
    body: Result<Json<CameraEventRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;
    let mut event = CameraEvent {
        id: None,
        camera_id: req.camera_id,
        event_type: req.event_type,
        confidence: req.confidence,
        snapshot_path: req.snapshot_path,
        metadata: req.metadata,
        acknowledged: false,
        created_at: chrono::Utc::now(),
    };
    let id = state
        .camera_storage
        .record_event(event.clone())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;
    // Publish the persisted event (now with its DB id) to the in-process bus (#91).
    if let Some(bus) = &state.event_bus {
        event.id = Some(id);
        bus.publish(BusEvent::Camera(event));
    }
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

async fn list_camera_events(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<CameraQueryParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let camera_id = params.camera_id.as_deref().unwrap_or("default");
    let limit = params.limit.unwrap_or(20).min(100);
    let events = state
        .camera_storage
        .list_events(camera_id, limit)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;
    let list: Vec<Value> = events
        .iter()
        .map(|e| {
            json!({
                "id":             e.id,
                "camera_id":      e.camera_id,
                "event_type":     e.event_type,
                "confidence":     e.confidence,
                "snapshot_path":  e.snapshot_path,
                "acknowledged":   e.acknowledged,
                "created_at":     e.created_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(json!({ "events": list })))
}

async fn acknowledge_camera_event(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state.camera_storage.acknowledge(id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    Ok(Json(json!({ "status": "ok" })))
}

/// Proxy multipart audio to the whisper.cpp server and return the transcript.
///
/// This route is intentionally public (no auth required). It is a local
/// development / testing tool and is only expected to be reachable from
/// localhost. Do not expose the GIAP server to the internet without adding
/// authentication to this endpoint.
async fn transcribe(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Read the "audio" field from the multipart body
    let mut audio_bytes: Option<Vec<u8>> = None;
    let mut filename = "audio.bin".to_string();
    let mut content_type = "audio/wav".to_string();

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("multipart error: {}", e)})),
        )
    })? {
        if field.name() == Some("audio") {
            filename = field.file_name().unwrap_or("audio.bin").to_string();
            content_type = field.content_type().unwrap_or("audio/wav").to_string();
            let bytes = field.bytes().await.map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("read error: {}", e)})),
                )
            })?;
            audio_bytes = Some(bytes.to_vec());
        }
    }

    let bytes = audio_bytes.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing 'audio' field in multipart body"})),
        )
    })?;

    // Forward to whisper.cpp /inference
    let whisper_url = format!("{}/inference", state.whisper_url);
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename)
        .mime_str(&content_type)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("MIME error: {}", e)})),
            )
        })?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "json");

    let resp = state
        .http_client
        .post(&whisper_url)
        .multipart(form)
        .send()
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("whisper server unreachable: {}", e)})),
            )
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!("whisper returned {}: {}", status, body);
        return Err((StatusCode::BAD_GATEWAY, Json(json!({"error": body}))));
    }

    let json: Value = resp.json().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("whisper response parse error: {}", e)})),
        )
    })?;

    let text = json["text"].as_str().unwrap_or("").trim().to_string();
    Ok(Json(json!({"text": text})))
}

// ── Wake-word calibration ─────────────────────────────────────────────────────

/// `POST /api/v1/voice/calibrate`
///
/// Accepts a WAV recording of the user saying their wake word/phrase, transcribes
/// it via whisper.cpp, and stores the result as a calibration variant in settings.
///
/// Call this 5 times (one per recording sample) during the onboarding WakeWord step.
/// The endpoint accumulates unique normalized variants and returns progress after each call.
///
/// **Request** — multipart/form-data with an `audio` field (WAV bytes, 16-bit mono 16 kHz).
///
/// **Response**
/// ```json
/// {
///   "transcript":    "hey goose",
///   "all_variants":  ["hey goose", "hey, goose", "a goose"],
///   "sample_count":  3,
///   "target_count":  5,
///   "complete":      false
/// }
/// ```
async fn calibrate_wake_word(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    const TARGET_SAMPLES: usize = 5;

    // ── Read audio field ─────────────────────────────────────────────────────
    let mut audio_bytes: Option<Vec<u8>> = None;
    let mut filename = "audio.wav".to_string();
    let mut content_type = "audio/wav".to_string();

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("multipart error: {e}")})),
        )
    })? {
        if field.name() == Some("audio") {
            filename = field.file_name().unwrap_or("audio.wav").to_string();
            content_type = field.content_type().unwrap_or("audio/wav").to_string();
            let bytes = field.bytes().await.map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("read error: {e}")})),
                )
            })?;
            audio_bytes = Some(bytes.to_vec());
        }
    }

    let bytes = audio_bytes.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing 'audio' field in multipart body"})),
        )
    })?;

    // ── Transcribe via whisper.cpp ───────────────────────────────────────────
    let whisper_url = format!("{}/inference", state.whisper_url);
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename)
        .mime_str(&content_type)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("MIME error: {e}")})),
            )
        })?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "json");

    let resp = state
        .http_client
        .post(&whisper_url)
        .multipart(form)
        .send()
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("whisper server unreachable: {e}")})),
            )
        })?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err((StatusCode::BAD_GATEWAY, Json(json!({"error": body}))));
    }

    let whisper_json: Value = resp.json().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("whisper parse error: {e}")})),
        )
    })?;

    let raw_transcript = whisper_json["text"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    if raw_transcript.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "no speech detected in recording"})),
        ));
    }

    // Normalize: strip punctuation, collapse whitespace, lowercase — same as detector.
    let normalized: String = raw_transcript
        .chars()
        .map(|c| if c.is_alphabetic() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    // ── Load settings, append variant, save ─────────────────────────────────
    let mut settings = state.settings_repo.get().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("settings load failed: {e}")})),
        )
    })?;

    // Append only if this normalized variant is not already present.
    if !settings
        .voice_wake_word_transcriptions
        .contains(&normalized)
    {
        settings
            .voice_wake_word_transcriptions
            .push(normalized.clone());
    }

    let all_variants = settings.voice_wake_word_transcriptions.clone();
    let sample_count = all_variants.len();
    let complete = sample_count >= TARGET_SAMPLES;

    state.settings_repo.update(&settings).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("settings save failed: {e}")})),
        )
    })?;

    Ok(Json(json!({
        "transcript":   raw_transcript,
        "normalized":   normalized,
        "all_variants": all_variants,
        "sample_count": sample_count,
        "target_count": TARGET_SAMPLES,
        "complete":     complete,
    })))
}

/// `DELETE /api/v1/voice/calibrate`
///
/// Clears all collected wake-word transcription variants, resetting calibration.
/// Safe to call at any point — the detector falls back to the raw normalized wake word.
async fn reset_wake_word_calibration(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut settings = state.settings_repo.get().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("settings load failed: {e}")})),
        )
    })?;

    settings.voice_wake_word_transcriptions.clear();

    state.settings_repo.update(&settings).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("settings save failed: {e}")})),
        )
    })?;

    Ok(Json(
        json!({"cleared": true, "message": "Wake-word calibration data cleared"}),
    ))
}

// ── Service connectivity test ─────────────────────────────────────────────────

/// `GET /api/v1/test`
///
/// Probes all external services in parallel and returns their status.
/// Use this to confirm whisper, llamafile, and ollama are reachable before
/// starting a voice session.
///
/// Response shape:
/// ```json
/// {
///   "whisper":   { "status": "ok",          "url": "...", "latency_ms": 12 },
///   "llamafile": { "status": "unavailable",  "url": "...", "error": "connection refused" },
///   "ollama":    { "status": "unavailable",  "url": "...", "error": "..." },
///   "llm":       { "status": "ok",           "provider": "llamafile → ollama", "response": "pong", "latency_ms": 220 }
/// }
/// ```
async fn test_services(State(state): State<Arc<AppState>>) -> Json<Value> {
    let client = &state.http_client;

    // Run all connectivity checks concurrently.
    let (whisper_result, llamafile_result, ollama_result) = tokio::join!(
        probe(client, &state.whisper_url, 3),
        probe(client, "http://127.0.0.1:8080", 3),
        probe(client, "http://127.0.0.1:11434", 3),
    );

    // Optionally probe the wired LLM provider with a real completion.
    let provider_opt = state.llm_provider.read().await.clone();
    let llm_result = if let Some(provider) = provider_opt {
        let model_name = provider.model_name();
        let t0 = std::time::Instant::now();
        let res = provider
            .complete(
                "You are a test service. Reply with exactly one word.",
                vec![ChatMessage::user("pong")],
            )
            .await;
        let ms = t0.elapsed().as_millis() as u64;
        match res {
            Ok(msg) => json!({
                "status": "ok",
                "provider": model_name,
                "response": msg.content.trim(),
                "latency_ms": ms,
            }),
            Err(e) => json!({
                "status": "error",
                "provider": model_name,
                "error": e.to_string(),
            }),
        }
    } else {
        json!({ "status": "not_configured" })
    };

    Json(json!({
        "whisper":   whisper_result,
        "llamafile": llamafile_result,
        "ollama":    ollama_result,
        "llm":       llm_result,
    }))
}

// ── Dev test HTML page ────────────────────────────────────────────────────────

const DEV_TEST_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>GIAP Dev Test</title>
<style>
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:monospace;background:#0d1117;color:#c9d1d9;padding:2rem}
h1{color:#58a6ff;margin-bottom:0.5rem}
.subtitle{color:#8b949e;font-size:0.8rem;margin-bottom:1.5rem}
h2{color:#79c0ff;font-size:0.95rem;margin-bottom:0.75rem}
.card{background:#161b22;border:1px solid #30363d;border-radius:6px;padding:1.25rem;margin-bottom:1.25rem}
.row{display:flex;gap:0.75rem;align-items:center;margin-bottom:0.5rem}
label{color:#8b949e;font-size:0.82rem;min-width:110px}
.badge{display:inline-block;padding:2px 10px;border-radius:12px;font-size:0.78rem;font-weight:bold}
.ok{background:#1a4731;color:#56d364}
.unavailable{background:#3d1a1a;color:#f85149}
.pending{background:#2d2a1e;color:#d29922}
.unknown{background:#21262d;color:#8b949e}
button{background:#238636;color:#fff;border:none;border-radius:4px;padding:5px 14px;cursor:pointer;font-family:monospace;font-size:0.85rem}
button:hover{background:#2ea043}
button:disabled{background:#333;color:#555;cursor:not-allowed}
button.danger{background:#b91c1c}
button.danger:hover{background:#dc2626}
button.danger.pulse{animation:pulse 1s infinite}
button.secondary{background:#21262d;border:1px solid #30363d}
button.secondary:hover{background:#30363d}
textarea,input[type=text]{width:100%;background:#0d1117;border:1px solid #30363d;border-radius:4px;color:#c9d1d9;padding:7px;font-family:monospace;font-size:0.85rem}
textarea{height:70px;resize:vertical}
pre{background:#0d1117;border:1px solid #21262d;border-radius:4px;padding:10px;font-size:0.78rem;overflow:auto;white-space:pre-wrap;word-break:break-all;max-height:180px;margin-top:0.5rem}
.ms{color:#8b949e;font-size:0.78rem}
.grid{display:grid;grid-template-columns:1fr 1fr;gap:1.25rem}
.note{color:#8b949e;font-size:0.78rem;margin-bottom:0.75rem}
@media(max-width:750px){.grid{grid-template-columns:1fr}}
@keyframes pulse{0%,100%{background:#b91c1c}50%{background:#ef4444}}
.flex{display:flex;gap:0.5rem;align-items:center;margin-bottom:0.75rem}
.detected{color:#56d364;font-weight:bold}
.not-detected{color:#f85149;font-weight:bold}
code{background:#21262d;padding:1px 5px;border-radius:3px;font-size:0.8rem}
</style>
</head>
<body>
<h1>🦆 GIAP Dev Test Panel</h1>
<p class="subtitle">⚠ Testing only — never expose this page to the internet.</p>

<!-- Services status -->
<div class="card">
  <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:0.75rem">
    <h2>Services</h2>
    <button onclick="checkServices()">Refresh</button>
  </div>
  <div style="display:grid;grid-template-columns:1fr 1fr;gap:0.25rem 1rem">
    <div class="row"><label>Whisper</label><span id="svc-whisper" class="badge unknown">—</span><span id="svc-whisper-ms" class="ms"></span></div>
    <div class="row"><label>Llamafile</label><span id="svc-llamafile" class="badge unknown">—</span><span id="svc-llamafile-ms" class="ms"></span></div>
    <div class="row"><label>Ollama</label><span id="svc-ollama" class="badge unknown">—</span><span id="svc-ollama-ms" class="ms"></span></div>
    <div class="row"><label>LLM Provider</label><span id="svc-llm" class="badge unknown">—</span><span id="svc-llm-ms" class="ms"></span></div>
  </div>
  <pre id="svc-detail" style="margin-top:0.5rem;display:none"></pre>
</div>

<div class="grid">
  <!-- Chat test -->
  <div class="card">
    <h2>Chat</h2>
    <p class="note">Requires onboarding completed. <button class="secondary" style="padding:2px 8px;font-size:0.75rem" onclick="doOnboard()">Auto-onboard</button></p>
    <div style="margin-bottom:0.5rem">
      <input type="text" id="chat-session" placeholder="session_id (blank = new)" style="margin-bottom:6px">
      <textarea id="chat-msg">What is 2+2?</textarea>
    </div>
    <div class="flex">
      <button onclick="sendChat()">Send</button>
      <button class="secondary" onclick="clearChat()">Clear</button>
    </div>
    <pre id="chat-out">—</pre>
  </div>

  <!-- Whisper transcription -->
  <div class="card">
    <h2>Whisper Transcription</h2>
    <p class="note">Records mic audio → <code>/api/v1/transcribe</code> → whisper.cpp</p>
    <div class="flex">
      <button id="rec-btn" onclick="toggleRecording()">● Record</button>
      <span id="rec-status" style="font-size:0.8rem;color:#8b949e"></span>
    </div>
    <pre id="transcribe-out">—</pre>
  </div>

  <!-- Wake word test -->
  <div class="card">
    <h2>Wake Word Test</h2>
    <p class="note">Records 3 s → transcribes → looks for <code>"goose"</code> in transcript</p>
    <div class="flex">
      <button id="wake-btn" onclick="testWakeWord()">▶ Test (3 s)</button>
      <span id="wake-status" style="font-size:0.8rem;color:#8b949e"></span>
    </div>
    <pre id="wake-out">Say "Goose" during the recording window.</pre>
  </div>

  <!-- Fallback provider -->
  <div class="card">
    <h2>Fallback Provider</h2>
    <p class="note">Sends a simple prompt through the wired LLM provider (includes fallback chain if configured).</p>
    <div class="flex">
      <button onclick="testFallback()">Test</button>
    </div>
    <pre id="fallback-out">—</pre>
  </div>

    <div class="card">
    <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:0.75rem">
      <h2>TTS</h2>
    </div>
    <p class="note">Plays audio on the <strong>server device</strong> speaker via <code>/api/v1/test/speak</code>. Piper TTS is the active backend.</p>

    <div style="margin-bottom:0.5rem">
      <textarea id="tts-text">Hello! I am Goose, your local AI assistant.</textarea>
    </div>
    <div class="flex">
      <button onclick="testTts()">▶ Speak on device</button>
    </div>
    <pre id="tts-out">—</pre>
  </div>

  <!-- Weather -->
  <div class="card" style="grid-column:1/-1">
    <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:0.75rem">
      <h2>Weather</h2>
      <div style="display:flex;gap:0.5rem;align-items:center">
        <span id="wx-cfg-badge" class="badge unknown">—</span>
        <button class="secondary" onclick="fetchWeather()">↺ Fetch</button>
        <button class="secondary" onclick="geolocate()">📍 My location</button>
      </div>
    </div>
    <p class="note">
      Weather is injected into every LLM system prompt when enabled.
      Enable via <code>PUT /api/v1/settings</code> with <code>weather_enabled:true</code>, <code>weather_latitude</code>, <code>weather_longitude</code>, <code>weather_location_name</code>.
    </p>
    <div style="display:grid;grid-template-columns:1fr 1fr 2fr;gap:0.5rem;margin-bottom:0.75rem">
      <div><label style="display:block;margin-bottom:2px;font-size:0.8rem">Latitude</label><input type="text" id="wx-lat" placeholder="-1.286"></div>
      <div><label style="display:block;margin-bottom:2px;font-size:0.8rem">Longitude</label><input type="text" id="wx-lon" placeholder="36.817"></div>
      <div><label style="display:block;margin-bottom:2px;font-size:0.8rem">Location name</label><input type="text" id="wx-loc" placeholder="Nairobi, KE"></div>
    </div>
    <div class="flex" style="margin-bottom:0.75rem">
      <button onclick="testWeatherDirect()">▶ Test (direct API call)</button>
      <button class="secondary" onclick="saveWeatherSettings()">💾 Save &amp; enable</button>
      <button class="secondary" onclick="disableWeather()">✕ Disable</button>
    </div>
    <div id="wx-display" style="display:none">
      <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(130px,1fr));gap:0.5rem;margin-bottom:0.75rem">
        <div style="background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:0.6rem;text-align:center">
          <div style="font-size:1.6rem;font-weight:bold;color:#79c0ff" id="wx-temp">—</div>
          <div style="font-size:0.7rem;color:#8b949e">Temperature</div>
        </div>
        <div style="background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:0.6rem;text-align:center">
          <div style="font-size:1.6rem;font-weight:bold;color:#79c0ff" id="wx-feels">—</div>
          <div style="font-size:0.7rem;color:#8b949e">Feels like</div>
        </div>
        <div style="background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:0.6rem;text-align:center">
          <div style="font-size:1.6rem;font-weight:bold;color:#79c0ff" id="wx-hum">—</div>
          <div style="font-size:0.7rem;color:#8b949e">Humidity</div>
        </div>
        <div style="background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:0.6rem;text-align:center">
          <div style="font-size:1.6rem;font-weight:bold;color:#79c0ff" id="wx-wind">—</div>
          <div style="font-size:0.7rem;color:#8b949e">Wind</div>
        </div>
        <div style="background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:0.6rem;text-align:center">
          <div style="font-size:1.6rem;font-weight:bold;color:#79c0ff" id="wx-precip">—</div>
          <div style="font-size:0.7rem;color:#8b949e">Precipitation</div>
        </div>
      </div>
      <div id="wx-desc" style="color:#56d364;font-size:0.9rem;margin-bottom:0.5rem"></div>
      <details>
        <summary style="cursor:pointer;color:#8b949e;font-size:0.78rem">LLM context block (what the assistant sees)</summary>
        <pre id="wx-context-block" style="margin-top:0.4rem"></pre>
      </details>
    </div>
    <pre id="wx-out" style="display:none"></pre>
  </div>

  <!-- Scheduler -->
  <div class="card" style="grid-column:1/-1">
    <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:0.75rem">
      <h2>Scheduler</h2>
      <button class="secondary" onclick="listSchedules()">↺ Refresh</button>
    </div>
    <p class="note">Cron uses 6-field format: <code>sec min hour dom month dow</code> — e.g. <code>0 0 8 * * *</code> = 08:00 daily.</p>

    <!-- Create form -->
    <details style="margin-bottom:0.75rem">
      <summary style="cursor:pointer;color:#79c0ff;font-size:0.85rem;margin-bottom:0.5rem">+ Create task</summary>
      <div style="display:grid;grid-template-columns:1fr 1fr;gap:0.5rem;margin-top:0.5rem">
        <div><label style="display:block;margin-bottom:2px">ID</label><input type="text" id="sched-id" placeholder="morning-summary"></div>
        <div><label style="display:block;margin-bottom:2px">Label</label><input type="text" id="sched-label" placeholder="Morning summary"></div>
        <div><label style="display:block;margin-bottom:2px">Cron (6-field)</label><input type="text" id="sched-cron" placeholder="0 0 8 * * *"></div>
        <div><label style="display:block;margin-bottom:2px">Webhook URL (optional)</label><input type="text" id="sched-webhook" placeholder="http://localhost:9999/hook"></div>
      </div>
      <div style="margin-top:0.5rem">
        <button onclick="createSchedule()">Create</button>
      </div>
    </details>

    <!-- Task list -->
    <div id="sched-list"><p style="color:#8b949e;font-size:0.82rem">Click Refresh to load tasks.</p></div>
    <pre id="sched-out" style="display:none;margin-top:0.5rem">—</pre>
  </div>

  <!-- Goose MCP Extensions -->
  <div class="card" style="grid-column:1/-1">
    <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:0.75rem">
      <h2>Goose MCP Extensions</h2>
      <div style="display:flex;gap:0.5rem;align-items:center">
        <span id="goose-badge" class="badge unknown">—</span>
        <button class="secondary" onclick="loadGooseStatus()">↺ Refresh</button>
      </div>
    </div>
    <p class="note">Enable with: <code>cargo run -p pond-server --features goose-agent -- serve --agent goose</code>.
    Extensions require onboarding. <button class="secondary" style="padding:2px 8px;font-size:0.75rem" onclick="doOnboard()">Auto-onboard</button></p>

    <!-- Extension list -->
    <div id="goose-ext-list" style="margin-bottom:0.75rem"><p style="color:#8b949e;font-size:0.82rem">Click Refresh to load extensions.</p></div>

    <!-- Add extension -->
    <details style="margin-bottom:0.75rem">
      <summary style="cursor:pointer;color:#79c0ff;font-size:0.85rem;margin-bottom:0.5rem">+ Add extension</summary>
      <div style="display:grid;grid-template-columns:1fr 1fr;gap:0.5rem;margin-top:0.5rem">
        <div><label style="display:block;margin-bottom:2px">Kind</label>
          <select id="ext-kind" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:4px;color:#c9d1d9;padding:7px;font-family:monospace;font-size:0.85rem" onchange="onExtKindChange()">
            <option value="builtin">builtin</option>
            <option value="stdio">stdio</option>
            <option value="streamable_http">streamable_http</option>
          </select>
        </div>
        <div><label style="display:block;margin-bottom:2px">Name</label><input type="text" id="ext-name" placeholder="giap"></div>
        <div id="ext-cmd-row"><label style="display:block;margin-bottom:2px">Command (stdio only)</label><input type="text" id="ext-cmd" placeholder="/usr/bin/my-mcp-server"></div>
        <div id="ext-uri-row" style="display:none"><label style="display:block;margin-bottom:2px">URI (http only)</label><input type="text" id="ext-uri" placeholder="http://localhost:8080/mcp"></div>
        <div style="grid-column:1/-1"><label style="display:block;margin-bottom:2px">Description</label><input type="text" id="ext-desc" placeholder="optional description"></div>
      </div>
      <div style="margin-top:0.5rem"><button onclick="addExtension()">Add</button></div>
    </details>

    <!-- Goose chat (uses existing /api/v1/chat, active only when --agent goose) -->
    <details>
      <summary style="cursor:pointer;color:#79c0ff;font-size:0.85rem;margin-bottom:0.5rem">▶ Test Goose chat</summary>
      <p class="note" style="margin-top:0.5rem">Sends through the wired agent. When <code>--agent goose</code> is active the Goose engine handles the message and can call MCP tools.</p>
      <input type="text" id="goose-session" placeholder="session_id (blank = new)" style="margin-bottom:6px">
      <textarea id="goose-msg" style="margin-bottom:6px">List my registered devices using the GIAP MCP tools.</textarea>
      <div class="flex"><button onclick="sendGooseChat()">Send</button></div>
      <pre id="goose-chat-out">—</pre>
    </details>

    <pre id="goose-out" style="display:none;margin-top:0.5rem">—</pre>
  </div>
</div>

<script>
const API='/api/v1';

// ── Onboarding ────────────────────────────────────────────────────────────────
async function doOnboard(){
  try{
    const r=await fetch(`${API}/onboard/complete`,{method:'POST'});
    const d=await r.json();
    if(d.status==='completed') location.reload();
    else alert(JSON.stringify(d));
  }catch(e){alert('Onboard error: '+e.message);}
}

// ── Services ──────────────────────────────────────────────────────────────────
async function checkServices(){
  ['whisper','llamafile','ollama','llm'].forEach(k=>setBadge(k,'pending','…'));
  try{
    const r=await fetch(`${API}/test`);
    const d=await r.json();
    renderSvc('whisper',d.whisper);
    renderSvc('llamafile',d.llamafile);
    renderSvc('ollama',d.ollama);
    renderLlm(d.llm);
    const det=document.getElementById('svc-detail');
    det.style.display='block';
    det.textContent=JSON.stringify(d,null,2);
  }catch(e){
    ['whisper','llamafile','ollama','llm'].forEach(k=>setBadge(k,'unavailable','error'));
  }
}
function renderSvc(k,s){
  if(!s)return setBadge(k,'unknown','?');
  setBadge(k,s.status==='ok'?'ok':'unavailable',s.status==='ok'?'ok':'unavailable');
  document.getElementById('svc-'+k+'-ms').textContent=s.latency_ms!=null?s.latency_ms+'ms':'';
}
function renderLlm(l){
  if(!l)return setBadge('llm','unknown','?');
  if(l.status==='not_configured')return setBadge('llm','unknown','not configured');
  setBadge('llm',l.status==='ok'?'ok':'unavailable',l.provider||l.status);
  document.getElementById('svc-llm-ms').textContent=l.latency_ms!=null?l.latency_ms+'ms':'';
}
function setBadge(k,cls,txt){
  const el=document.getElementById('svc-'+k);
  el.className='badge '+cls;el.textContent=txt;
}

// ── Chat ──────────────────────────────────────────────────────────────────────
async function sendChat(){
  const msg=document.getElementById('chat-msg').value.trim();
  if(!msg)return;
  const si=document.getElementById('chat-session');
  const out=document.getElementById('chat-out');
  out.textContent='…';
  const body={message:msg};
  if(si.value.trim())body.session_id=si.value.trim();
  try{
    const r=await fetch(`${API}/chat`,{
      method:'POST',
      headers:{'Content-Type':'application/json','Authorization':'Bearer dev'},
      body:JSON.stringify(body)
    });
    const d=await r.json();
    if(d.session_id)si.value=d.session_id;
    out.textContent=JSON.stringify(d,null,2);
  }catch(e){out.textContent='Error: '+e.message;}
}
function clearChat(){
  document.getElementById('chat-session').value='';
  document.getElementById('chat-out').textContent='—';
}

// ── WAV encoder (browser → whisper.cpp requires 16-bit PCM WAV, 16 kHz mono) ──
// Browser MediaRecorder produces WebM/Opus which whisper.cpp cannot decode.
// We decode via AudioContext, resample to 16 kHz mono, then write a WAV header.
async function blobToWav(blob){
  const ab=await blob.arrayBuffer();
  const ctx=new AudioContext();
  let decoded;
  try{decoded=await ctx.decodeAudioData(ab);}
  finally{ctx.close();}
  const SR=16000;
  const len=Math.ceil(decoded.duration*SR);
  const off=new OfflineAudioContext(1,len,SR);
  const src=off.createBufferSource();
  src.buffer=decoded;
  src.connect(off.destination);
  src.start(0);
  const rendered=await off.startRendering();
  const pcmF32=rendered.getChannelData(0);
  const pcm16=new Int16Array(pcmF32.length);
  for(let i=0;i<pcmF32.length;i++){
    const s=Math.max(-1,Math.min(1,pcmF32[i]));
    pcm16[i]=s<0?s*0x8000:s*0x7fff;
  }
  // Build WAV container
  const buf=new ArrayBuffer(44+pcm16.byteLength);
  const v=new DataView(buf);
  const str=(o,s)=>{for(let i=0;i<s.length;i++)v.setUint8(o+i,s.charCodeAt(i));};
  str(0,'RIFF');v.setUint32(4,36+pcm16.byteLength,true);str(8,'WAVE');
  str(12,'fmt ');v.setUint32(16,16,true);v.setUint16(20,1,true);v.setUint16(22,1,true);
  v.setUint32(24,SR,true);v.setUint32(28,SR*2,true);v.setUint16(32,2,true);v.setUint16(34,16,true);
  str(36,'data');v.setUint32(40,pcm16.byteLength,true);
  new Int16Array(buf,44).set(pcm16);
  return new Blob([buf],{type:'audio/wav'});
}

// ── Recording helpers ─────────────────────────────────────────────────────────
let mr=null,chunks=[];
async function toggleRecording(){
  if(mr&&mr.state==='recording'){mr.stop();return;}
  chunks=[];
  try{
    const stream=await navigator.mediaDevices.getUserMedia({audio:true});
    mr=new MediaRecorder(stream);
    mr.ondataavailable=e=>chunks.push(e.data);
    mr.onstop=async()=>{
      stream.getTracks().forEach(t=>t.stop());
      const raw=new Blob(chunks,{type:mr.mimeType});
      transcribeBlob(raw,'transcribe-out');
      setRecBtn(false);
    };
    mr.start();setRecBtn(true);
  }catch(e){document.getElementById('transcribe-out').textContent='Mic error: '+e.message;}
}
function setRecBtn(on){
  const b=document.getElementById('rec-btn');
  const s=document.getElementById('rec-status');
  if(on){b.textContent='■ Stop';b.classList.add('danger','pulse');s.textContent='Recording…';}
  else{b.textContent='● Record';b.className='';s.textContent='';}
}
async function transcribeBlob(raw,outId){
  const out=document.getElementById(outId);
  out.textContent='Converting to WAV…';
  let wav;
  try{wav=await blobToWav(raw);}
  catch(e){out.textContent='WAV encode error: '+e.message;return '';}
  out.textContent='Transcribing…';
  const form=new FormData();
  form.append('audio',wav,'audio.wav');
  try{
    const r=await fetch(`${API}/transcribe`,{method:'POST',body:form});
    const d=await r.json();
    out.textContent=JSON.stringify(d,null,2);
    return d.text||'';
  }catch(e){out.textContent='Error: '+e.message;return '';}
}

// ── Wake word ─────────────────────────────────────────────────────────────────
async function testWakeWord(){
  const btn=document.getElementById('wake-btn');
  const st=document.getElementById('wake-status');
  const out=document.getElementById('wake-out');
  btn.disabled=true;out.textContent='…';
  let secs=3;
  st.textContent=`Recording ${secs}s… say "Goose"`;
  const tick=setInterval(()=>{secs--;if(secs>0)st.textContent=`Recording ${secs}s… say "Goose"`;},1000);
  try{
    const stream=await navigator.mediaDevices.getUserMedia({audio:true});
    const wrec=new MediaRecorder(stream);
    const wchunks=[];
    wrec.ondataavailable=e=>wchunks.push(e.data);
    wrec.onstop=async()=>{
      clearInterval(tick);st.textContent='Converting…';
      stream.getTracks().forEach(t=>t.stop());
      const raw=new Blob(wchunks,{type:wrec.mimeType});
      let wav;
      try{wav=await blobToWav(raw);}
      catch(e){out.textContent='WAV encode error: '+e.message;st.textContent='';btn.disabled=false;return;}
      st.textContent='Transcribing…';
      const form=new FormData();form.append('audio',wav,'audio.wav');
      try{
        const r=await fetch(`${API}/transcribe`,{method:'POST',body:form});
        const d=await r.json();
        const text=(d.text||'').toLowerCase();
        const hit=text.includes('goose');
        out.textContent=`Transcript: "${d.text||'(empty)'}"\n\nWake word "goose": ${hit?'✅ DETECTED':'❌ not found'}`;
        st.innerHTML=hit?'<span class="detected">✅ Detected</span>':'<span class="not-detected">❌ Not detected</span>';
      }catch(e){out.textContent='Error: '+e.message;st.textContent='';}
      btn.disabled=false;
    };
    wrec.start();
    setTimeout(()=>wrec.stop(),3000);
  }catch(e){clearInterval(tick);out.textContent='Mic error: '+e.message;st.textContent='';btn.disabled=false;}
}

// ── Fallback provider ─────────────────────────────────────────────────────────
async function testFallback(){
  const out=document.getElementById('fallback-out');
  out.textContent='Testing…';
  const t0=Date.now();
  try{
    const r=await fetch(`${API}/chat`,{
      method:'POST',
      headers:{'Content-Type':'application/json','Authorization':'Bearer dev'},
      body:JSON.stringify({message:'Reply with exactly one word: pong'})
    });
    const d=await r.json();
    d._latency_ms=Date.now()-t0;
    out.textContent=JSON.stringify(d,null,2);
  }catch(e){out.textContent='Error: '+e.message;}
}

async function testTts(){
  const text=document.getElementById('tts-text').value.trim()||'Hello from Goose!';
  const out=document.getElementById('tts-out');
  out.textContent='Speaking…';
  try{
    const t0=Date.now();
    const r=await fetch(`${API}/test/speak`,{
      method:'POST',
      headers:{'Content-Type':'application/json'},
      body:JSON.stringify({text})
    });
    const d=await r.json();
    d._client_latency_ms=Date.now()-t0;
    out.textContent=JSON.stringify(d,null,2);
  }catch(e){out.textContent='Error: '+e.message;}
}

// ── Weather ───────────────────────────────────────────────────────────────────
async function fetchWeather(){
  const badge=document.getElementById('wx-cfg-badge');
  const out=document.getElementById('wx-out');
  badge.className='badge pending';badge.textContent='…';
  out.style.display='none';
  try{
    const r=await fetch(`${API}/weather`);
    if(r.status===503){
      badge.className='badge unavailable';badge.textContent='disabled';
      document.getElementById('wx-display').style.display='none';
      out.style.display='block';out.textContent='Weather disabled — fill in lat/lon and click "Save & enable".';
      return;
    }
    const d=await r.json();
    if(d.error){
      badge.className='badge unavailable';badge.textContent='error';
      out.style.display='block';out.textContent=d.error;
      return;
    }
    badge.className='badge ok';badge.textContent='ok';
    renderWeather(d);
  }catch(e){
    badge.className='badge unavailable';badge.textContent='error';
    out.style.display='block';out.textContent='Error: '+e.message;
  }
}

async function testWeatherDirect(){
  const lat=parseFloat(document.getElementById('wx-lat').value);
  const lon=parseFloat(document.getElementById('wx-lon').value);
  const loc=document.getElementById('wx-loc').value.trim()||`${lat}, ${lon}`;
  const out=document.getElementById('wx-out');
  if(isNaN(lat)||isNaN(lon)){alert('Enter valid lat/lon first.');return;}
  out.style.display='block';out.textContent='Fetching from open-meteo.com…';
  document.getElementById('wx-display').style.display='none';
  try{
    const url=`https://api.open-meteo.com/v1/forecast?latitude=${lat}&longitude=${lon}`+
      `&current=temperature_2m,relative_humidity_2m,apparent_temperature,precipitation,`+
      `weather_code,wind_speed_10m,wind_direction_10m`+
      `&temperature_unit=celsius&wind_speed_unit=kmh&precipitation_unit=mm`;
    const r=await fetch(url);
    const api=await r.json();
    const c=api.current;
    const WMO={0:'Clear sky',1:'Mainly clear',2:'Partly cloudy',3:'Overcast',
      45:'Fog',48:'Fog',51:'Light drizzle',53:'Moderate drizzle',55:'Dense drizzle',
      61:'Slight rain',63:'Moderate rain',65:'Heavy rain',
      71:'Slight snow',73:'Moderate snow',75:'Heavy snow',
      80:'Slight showers',81:'Moderate showers',82:'Violent showers',
      95:'Thunderstorm',96:'Thunderstorm with hail',99:'Thunderstorm with hail'};
    const desc=WMO[c.weather_code]||'Unknown';
    const data={
      temperature_c:c.temperature_2m,feels_like_c:c.apparent_temperature,
      humidity_pct:c.relative_humidity_2m,description:desc,
      wind_speed_kmh:c.wind_speed_10m,wind_direction_deg:c.wind_direction_10m,
      precipitation_mm:c.precipitation,location_name:loc,
      fetched_at:new Date().toISOString()
    };
    renderWeather(data);
    out.textContent=JSON.stringify(api.current,null,2);
  }catch(e){out.textContent='Error: '+e.message;}
}

function renderWeather(d){
  document.getElementById('wx-display').style.display='block';
  document.getElementById('wx-temp').textContent=d.temperature_c.toFixed(1)+'°C';
  document.getElementById('wx-feels').textContent=d.feels_like_c.toFixed(1)+'°C';
  document.getElementById('wx-hum').textContent=d.humidity_pct+'%';
  document.getElementById('wx-wind').textContent=d.wind_speed_kmh.toFixed(0)+' km/h';
  document.getElementById('wx-precip').textContent=d.precipitation_mm.toFixed(1)+' mm';
  document.getElementById('wx-desc').textContent=`${d.description} — ${d.location_name}`;
  const block=`[Current Weather — ${d.location_name}]\n${d.description} | `+
    `${d.temperature_c.toFixed(1)}°C (feels like ${d.feels_like_c.toFixed(1)}°C) | `+
    `Humidity: ${d.humidity_pct}% | Wind: ${d.wind_speed_kmh.toFixed(0)} km/h | `+
    `Precip: ${d.precipitation_mm.toFixed(1)} mm`;
  document.getElementById('wx-context-block').textContent=block;
}

function geolocate(){
  if(!navigator.geolocation){alert('Geolocation not supported.');return;}
  navigator.geolocation.getCurrentPosition(pos=>{
    document.getElementById('wx-lat').value=pos.coords.latitude.toFixed(6);
    document.getElementById('wx-lon').value=pos.coords.longitude.toFixed(6);
  },err=>alert('Geolocation error: '+err.message));
}

async function saveWeatherSettings(){
  const lat=parseFloat(document.getElementById('wx-lat').value);
  const lon=parseFloat(document.getElementById('wx-lon').value);
  const loc=document.getElementById('wx-loc').value.trim();
  if(isNaN(lat)||isNaN(lon)){alert('Enter valid lat/lon first.');return;}
  const out=document.getElementById('wx-out');
  out.style.display='block';out.textContent='Saving…';
  try{
    const r=await fetch(`${API}/settings`,{
      method:'PUT',
      headers:{'Content-Type':'application/json'},
      body:JSON.stringify({weather_enabled:true,weather_latitude:lat,weather_longitude:lon,weather_location_name:loc})
    });
    const d=await r.json();
    if(r.ok){out.textContent='Saved! Fetching weather…';fetchWeather();}
    else{out.textContent='Error: '+JSON.stringify(d);}
  }catch(e){out.textContent='Error: '+e.message;}
}

async function disableWeather(){
  const out=document.getElementById('wx-out');
  out.style.display='block';out.textContent='Disabling…';
  try{
    const r=await fetch(`${API}/settings`,{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({weather_enabled:false})});
    const d=await r.json();
    if(r.ok){out.textContent='Weather disabled.';fetchWeather();}
    else{out.textContent='Error: '+JSON.stringify(d);}
  }catch(e){out.textContent='Error: '+e.message;}
}

// ── Scheduler ─────────────────────────────────────────────────────────────────
async function listSchedules(){
  const out=document.getElementById('sched-out');
  const list=document.getElementById('sched-list');
  try{
    const r=await fetch(`${API}/schedules`);
    if(r.status===503){
      list.innerHTML='<p style="color:#f85149;font-size:0.82rem">Scheduler not configured (503).</p>';
      return;
    }
    const tasks=await r.json();
    if(!Array.isArray(tasks)||tasks.length===0){
      list.innerHTML='<p style="color:#8b949e;font-size:0.82rem">No scheduled tasks.</p>';
      return;
    }
    list.innerHTML=tasks.map(t=>`
      <div style="border:1px solid #30363d;border-radius:4px;padding:0.6rem 0.75rem;margin-bottom:0.5rem;display:flex;justify-content:space-between;align-items:center;gap:0.5rem;flex-wrap:wrap">
        <div style="flex:1;min-width:160px">
          <span style="font-weight:bold;font-size:0.88rem">${esc(t.id)}</span>
          <span style="color:#8b949e;font-size:0.78rem;margin-left:6px">${esc(t.label)}</span><br>
          <code style="font-size:0.75rem">${esc(t.cron)}</code>
          ${t.paused?'<span class="badge pending" style="margin-left:6px">paused</span>':''}
          ${t.currently_running?'<span class="badge ok" style="margin-left:6px">running</span>':''}
        </div>
        <div style="display:flex;gap:0.4rem;flex-wrap:wrap">
          <button class="secondary" style="padding:3px 8px;font-size:0.75rem" onclick="schedRunNow('${esc(t.id)}')">▶ Run now</button>
          ${t.paused
            ?`<button class="secondary" style="padding:3px 8px;font-size:0.75rem" onclick="schedResume('${esc(t.id)}')">Resume</button>`
            :`<button class="secondary" style="padding:3px 8px;font-size:0.75rem" onclick="schedPause('${esc(t.id)}')">Pause</button>`}
          <button class="danger" style="padding:3px 8px;font-size:0.75rem" onclick="schedDelete('${esc(t.id)}')">Delete</button>
        </div>
      </div>`).join('');
    out.style.display='none';
  }catch(e){
    list.innerHTML='';
    out.style.display='block';
    out.textContent='Error: '+e.message;
  }
}

function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}

async function schedAction(method,path,body){
  const out=document.getElementById('sched-out');
  out.style.display='block';
  out.textContent='…';
  try{
    const opts={method,headers:{'Content-Type':'application/json'}};
    if(body)opts.body=JSON.stringify(body);
    const r=await fetch(`${API}${path}`,opts);
    const d=await r.json();
    out.textContent=JSON.stringify(d,null,2);
    listSchedules();
  }catch(e){out.textContent='Error: '+e.message;}
}

async function createSchedule(){
  const id=document.getElementById('sched-id').value.trim();
  const label=document.getElementById('sched-label').value.trim()||id;
  const cron=document.getElementById('sched-cron').value.trim();
  const webhook=document.getElementById('sched-webhook').value.trim();
  if(!id||!cron){alert('ID and Cron are required.');return;}
  const payload=webhook?{webhook_url:webhook}:{};
  await schedAction('POST','/schedules',{id,label,cron,payload});
}
async function schedRunNow(id){await schedAction('POST',`/schedules/${id}/run-now`);}
async function schedPause(id){await schedAction('POST',`/schedules/${id}/pause`);}
async function schedResume(id){await schedAction('POST',`/schedules/${id}/resume`);}
async function schedDelete(id){
  if(!confirm(`Delete task "${id}"?`))return;
  await schedAction('DELETE',`/schedules/${id}`);
}

// ── Goose MCP Extensions ──────────────────────────────────────────────────────
async function loadGooseStatus(){
  const badge=document.getElementById('goose-badge');
  const list=document.getElementById('goose-ext-list');
  const out=document.getElementById('goose-out');
  badge.textContent='…';badge.className='badge pending';
  try{
    const r=await fetch(`${API}/dev/goose`);
    const d=await r.json();
    if(d.goose_active){
      badge.textContent='active';badge.className='badge ok';
      if(d.extensions&&d.extensions.length>0){
        list.innerHTML=d.extensions.map(e=>`
          <div style="border:1px solid #30363d;border-radius:4px;padding:0.5rem 0.75rem;margin-bottom:0.4rem;display:flex;justify-content:space-between;align-items:center">
            <div>
              <span style="font-weight:bold;font-size:0.88rem">${esc(e.name)}</span>
              <span class="badge unknown" style="margin-left:6px;font-size:0.72rem">${esc(e.kind)}</span>
              ${e.tools&&e.tools.length?'<br><span style="color:#8b949e;font-size:0.75rem">tools: '+e.tools.map(t=>esc(t)).join(', ')+'</span>':''}
            </div>
            <button class="danger" style="padding:3px 8px;font-size:0.75rem" onclick="removeExtension('${esc(e.name)}')">Remove</button>
          </div>`).join('');
      } else {
        list.innerHTML='<p style="color:#8b949e;font-size:0.82rem">No extensions loaded. Add the "giap" builtin to enable GIAP tools.</p>';
      }
      out.style.display='none';
    } else {
      badge.textContent='inactive';badge.className='badge unavailable';
      list.innerHTML=`<p style="color:#f85149;font-size:0.82rem">${esc(d.message||'Goose agent not active.')}</p>`;
      out.style.display='none';
    }
  }catch(e){badge.textContent='error';badge.className='badge unavailable';out.style.display='block';out.textContent='Error: '+e.message;}
}

function onExtKindChange(){
  const kind=document.getElementById('ext-kind').value;
  document.getElementById('ext-cmd-row').style.display=kind==='stdio'?'':'none';
  document.getElementById('ext-uri-row').style.display=kind==='streamable_http'?'':'none';
}

async function addExtension(){
  const out=document.getElementById('goose-out');
  const kind=document.getElementById('ext-kind').value;
  const name=document.getElementById('ext-name').value.trim();
  const desc=document.getElementById('ext-desc').value.trim();
  const cmd=document.getElementById('ext-cmd').value.trim();
  const uri=document.getElementById('ext-uri').value.trim();
  if(!name){alert('Name is required.');return;}
  const body={kind,name,description:desc,args:[],env:{}};
  if(kind==='stdio'){if(!cmd){alert('Command is required for stdio.');return;}body.command=cmd;}
  if(kind==='streamable_http'){if(!uri){alert('URI is required for http.');return;}body.uri=uri;}
  out.style.display='block';out.textContent='Adding…';
  try{
    const r=await fetch(`${API}/extensions`,{
      method:'POST',
      headers:{'Content-Type':'application/json','Authorization':'Bearer dev'},
      body:JSON.stringify(body)
    });
    const d=await r.json();
    out.textContent=JSON.stringify(d,null,2);
    loadGooseStatus();
  }catch(e){out.textContent='Error: '+e.message;}
}

async function removeExtension(name){
  if(!confirm(`Remove extension "${name}"?`))return;
  const out=document.getElementById('goose-out');
  out.style.display='block';out.textContent='Removing…';
  try{
    const r=await fetch(`${API}/extensions/${encodeURIComponent(name)}`,{
      method:'DELETE',
      headers:{'Authorization':'Bearer dev'}
    });
    out.textContent=r.ok?'Removed.':JSON.stringify(await r.json(),null,2);
    loadGooseStatus();
  }catch(e){out.textContent='Error: '+e.message;}
}

async function sendGooseChat(){
  const si=document.getElementById('goose-session');
  const msg=document.getElementById('goose-msg').value.trim();
  const out=document.getElementById('goose-chat-out');
  if(!msg)return;
  out.textContent='Thinking…';
  const body={message:msg};
  if(si.value.trim())body.session_id=si.value.trim();
  try{
    const r=await fetch(`${API}/chat`,{
      method:'POST',
      headers:{'Content-Type':'application/json','Authorization':'Bearer dev'},
      body:JSON.stringify(body)
    });
    const d=await r.json();
    if(d.session_id)si.value=d.session_id;
    out.textContent=JSON.stringify(d,null,2);
  }catch(e){out.textContent='Error: '+e.message;}
}

// ── Init ──────────────────────────────────────────────────────────────────────
checkServices();
fetchWeather();
listSchedules();
loadGooseStatus();
</script>
</body>
</html>"#;

/// `GET /dev/test` — self-contained HTML dev test panel.
///
/// Tests whisper, llamafile, ollama, fallback provider, wake word, and chat.
/// **Never expose this to the internet.**
pub async fn dev_test_page() -> Html<&'static str> {
    Html(DEV_TEST_HTML)
}

/// `GET /dev/face` — self-contained webcam page that exercises every face
/// endpoint added in phase-2: enroll-quality, register, identify, identify-
/// burst, and the per-profile threshold + diagnostic routes.  Requires the
/// browser to grant camera access.  **Never expose this to the internet.**
pub async fn dev_face_page() -> Html<&'static str> {
    Html(DEV_FACE_HTML)
}

const DEV_FACE_HTML: &str = r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>pond — face dev panel</title>
<style>
 body{font-family:-apple-system,system-ui,sans-serif;margin:0;background:#0b1020;color:#e6e8f2;padding:18px;}
 h1{font-size:18px;margin:0 0 12px;font-weight:600;}
 .row{display:flex;gap:18px;flex-wrap:wrap;}
 .card{background:#161c33;border-radius:10px;padding:14px;min-width:320px;flex:1;}
 video{width:100%;background:#000;border-radius:8px;}
 button{background:#3b82f6;color:#fff;border:0;border-radius:6px;padding:8px 12px;font-weight:600;cursor:pointer;margin:4px 4px 4px 0;}
 button:hover{background:#2563eb;}
 button.warn{background:#b45309;}
 button.danger{background:#b91c1c;}
 input,select{background:#0f1530;color:#e6e8f2;border:1px solid #2a3460;padding:6px;border-radius:5px;}
 pre{background:#0f1530;border-radius:6px;padding:10px;font-size:12px;max-height:260px;overflow:auto;}
 .ok{color:#22c55e;} .bad{color:#ef4444;} .dim{color:#9ca3af;}
 .pill{display:inline-block;padding:2px 8px;border-radius:999px;background:#1e293b;font-size:11px;margin-left:6px;}
</style></head><body>
<h1>🎥 Face recognition dev panel <span class="pill" id="status">starting…</span></h1>
<div class="row">
  <div class="card">
    <h3>Camera</h3>
    <video id="cam" autoplay playsinline muted></video>
    <div style="margin-top:8px;">
      <label>Profile: <select id="profile"></select></label>
      <button id="refreshProfiles">↻</button>
      <button id="newProfile">+ new</button>
    </div>
    <div style="margin-top:6px;">
      <button id="quality">Pre-flight (enroll-quality)</button>
      <button id="enroll">Enroll one frame</button>
      <button id="enroll5" class="warn">Enroll 5 frames</button>
    </div>
    <div style="margin-top:6px;">
      <button id="identify">Identify (live, 5-frame burst)</button>
      <button id="burst" class="dim" title="Same pipeline — kept for back-compat">Identify burst</button>
    </div>
    <div style="margin-top:6px;">
      <button id="pairwise" class="dim">/debug/pairwise</button>
      <button id="evalbtn" class="dim">/debug/eval</button>
      <button id="forget" class="danger">Forget biometrics</button>
    </div>
  </div>
  <div class="card">
    <h3>Result</h3>
    <pre id="out">(no calls yet)</pre>
  </div>
</div>
<script>
const API='/api/v1';
const $=id=>document.getElementById(id);
const log=o=>$('out').textContent=(typeof o==='string'?o:JSON.stringify(o,null,2));
let stream=null,sel=()=>$('profile').value;

async function init(){
  try{
    stream=await navigator.mediaDevices.getUserMedia({video:{width:640,height:480}});
    $('cam').srcObject=stream;
    $('status').textContent='camera live';$('status').classList.add('ok');
  }catch(e){$('status').textContent='camera blocked';$('status').classList.add('bad');log(e.message);}
  await refreshProfiles();
}

async function refreshProfiles(){
  const r=await fetch(API+'/profiles').then(r=>r.json()).catch(e=>({error:e.message}));
  const list=Array.isArray(r)?r:(r.profiles||[]);
  const sel=$('profile');sel.innerHTML='';
  list.forEach(p=>{const o=document.createElement('option');o.value=p.id;o.textContent=`${p.name||p.display_name||p.id} (${p.id.slice(0,8)})`;sel.appendChild(o);});
  if(!list.length){const o=document.createElement('option');o.value='';o.textContent='(no profiles — click "+ new")';sel.appendChild(o);}
}

async function newProfile(){
  const name=prompt('Display name?');if(!name)return;
  const r=await fetch(API+'/profiles',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({display_name:name})}).then(r=>r.json());
  log(r);await refreshProfiles();
}

function grabFrame(){
  const v=$('cam'),c=document.createElement('canvas');c.width=v.videoWidth;c.height=v.videoHeight;
  c.getContext('2d').drawImage(v,0,0);
  return new Promise(res=>c.toBlob(res,'image/jpeg',0.9));
}

async function postMultipart(path,fields){
  const fd=new FormData();
  for(const[k,v]of Object.entries(fields)){
    if(Array.isArray(v))v.forEach(x=>fd.append(k,x));else fd.append(k,v);
  }
  const r=await fetch(API+path,{method:'POST',body:fd});return r.json();
}

$('refreshProfiles').onclick=refreshProfiles;
$('newProfile').onclick=newProfile;
$('quality').onclick=async()=>{const f=await grabFrame();log(await postMultipart('/faces/enroll-quality',{image:f}));};
$('enroll').onclick=async()=>{const pid=sel();if(!pid)return alert('select profile');const f=await grabFrame();log(await postMultipart('/faces/register',{profile_id:pid,image:f}));};
$('enroll5').onclick=async()=>{
  const pid=sel();if(!pid)return alert('select profile');
  const out=[];for(let i=0;i<5;i++){await new Promise(r=>setTimeout(r,700));const f=await grabFrame();out.push(await postMultipart('/faces/register',{profile_id:pid,image:f}));log({progress:`${i+1}/5`,latest:out[out.length-1]});}
  log({enrolled:out.length,results:out});
};
// Production-style identify: always multi-frame with liveness gates.
// A held-up photo yields near-identical embeddings + zero landmark motion
// across the burst and trips `reason: "liveness_failed"` — which a single
// frame cannot detect.  The old single-frame endpoint (/faces/identify)
// still exists server-side for API callers, but the dev UI no longer
// exposes it.
$('identify').onclick=async()=>{
  const frames=[];for(let i=0;i<5;i++){await new Promise(r=>setTimeout(r,400));frames.push(await grabFrame());}
  log(await postMultipart('/faces/identify-burst',{image:frames}));
};
$('burst').onclick=async()=>{
  const frames=[];for(let i=0;i<5;i++){await new Promise(r=>setTimeout(r,400));frames.push(await grabFrame());}
  log(await postMultipart('/faces/identify-burst',{image:frames}));
};
$('pairwise').onclick=async()=>log(await fetch(API+'/faces/debug/pairwise').then(r=>r.json()));
$('evalbtn').onclick=async()=>log(await fetch(API+'/faces/debug/eval').then(r=>r.json()));
$('forget').onclick=async()=>{const pid=sel();if(!pid)return;if(!confirm('Forget all biometrics for '+pid+'?'))return;const r=await fetch(API+`/users/${pid}/biometrics`,{method:'DELETE'});log(await r.json());};

init();
</script>
</body></html>"#;

/// `GET /api/v1/dev/goose` — Goose agent status (public, dev only).
///
/// Returns whether the Goose agent is active and the extension manager is wired.
/// When active, also returns the current tool list.
async fn goose_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    match &state.extension_manager {
        None => Json(json!({
            "goose_active": false,
            "message": "Goose agent not active. Restart with: cargo run -p pond-server --features goose-agent -- serve --agent goose"
        })),
        Some(mgr) => {
            let tools = mgr.list_tools().await.unwrap_or_default();
            let extensions = mgr.list_extensions().await.unwrap_or_default();
            Json(json!({
                "goose_active": true,
                "extension_count": extensions.len(),
                "extensions": extensions.iter().map(|e| json!({
                    "name": e.name,
                    "kind": e.kind,
                    "tools": e.tools,
                })).collect::<Vec<_>>(),
                "tool_count": tools.len(),
                "tools": tools,
            }))
        }
    }
}

/// `POST /api/v1/test/speak`
///
/// Synthesise speech on the server device via the configured TTS engine.
/// Body: `{ "text": "hello world" }`
/// Response: `{ "status": "ok"|"unavailable", "engine": "piper"|"print"|"none", "text": "..." }`
async fn test_speak(
    State(state): State<Arc<AppState>>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Json<Value> {
    let text = match body {
        Ok(Json(v)) => v
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("Hello from Goose In A Pond!")
            .to_string(),
        Err(_) => "Hello from Goose In A Pond!".to_string(),
    };

    match &state.tts {
        Some(tts) => {
            let t0 = std::time::Instant::now();
            match tts.speak(&text).await {
                Ok(()) => Json(json!({
                    "status": "ok",
                    "text": text,
                    "latency_ms": t0.elapsed().as_millis() as u64,
                })),
                Err(e) => Json(json!({
                    "status": "error",
                    "text": text,
                    "error": e.to_string(),
                })),
            }
        }
        None => Json(json!({
            "status": "unavailable",
            "text": text,
            "message": "No TTS engine configured. Start server with --tts piper after running setup.",
        })),
    }
}

/// Hit `url` with a GET, return a status/latency object.
async fn probe(client: &reqwest::Client, url: &str, timeout_secs: u64) -> Value {
    let t0 = std::time::Instant::now();
    match client
        .get(url)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .send()
        .await
    {
        Ok(_) => json!({
            "status": "ok",
            "url": url,
            "latency_ms": t0.elapsed().as_millis() as u64,
        }),
        Err(e) => json!({
            "status": "unavailable",
            "url": url,
            "error": e.to_string(),
        }),
    }
}

// ───────────────────────── Scheduler Handlers ───────────────────────

/// `GET /api/v1/schedules` — list all scheduled tasks.
async fn list_schedules(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    match scheduler.list_tasks().await {
        Ok(tasks) => (StatusCode::OK, Json(json!(tasks))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

/// API-level create schedule request — supports both the new format (`kind`,
/// `timezone`) and the legacy format (`prompt` + `payload`).
#[derive(Debug, serde::Deserialize)]
struct ApiCreateScheduleRequest {
    /// Optional: if absent, a UUID is generated.
    id: Option<String>,
    /// Human-readable name.  Falls back to `label` for backward compat.
    #[serde(alias = "label")]
    name: String,
    cron: String,
    /// The prompt to send to the agent (shorthand for AgentPrompt kind).
    prompt: Option<String>,
    /// IANA timezone.  Defaults to "UTC" if absent.
    timezone: Option<String>,
    /// Explicit kind — if omitted, inferred from `prompt` or `payload`.
    kind: Option<TaskKind>,
    /// Legacy field: `{"webhook_url": "..."}` or `{"prompt": "..."}`.
    payload: Option<serde_json::Value>,
}

/// `POST /api/v1/schedules` — create a new scheduled task.
async fn create_schedule(
    State(state): State<Arc<AppState>>,
    result: Result<Json<ApiCreateScheduleRequest>, JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    let Json(api_req) = match result {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
        }
    };

    // Resolve task kind: explicit `kind` > `prompt` field > legacy `payload`.
    let kind = if let Some(k) = api_req.kind {
        k
    } else if let Some(prompt) = api_req.prompt {
        TaskKind::AgentPrompt { prompt }
    } else if let Some(payload) = &api_req.payload {
        if let Some(url) = payload.get("webhook_url").and_then(|v| v.as_str()) {
            TaskKind::Webhook {
                webhook_url: url.to_string(),
            }
        } else if let Some(p) = payload.get("prompt").and_then(|v| v.as_str()) {
            TaskKind::AgentPrompt {
                prompt: p.to_string(),
            }
        } else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "missing 'prompt', 'kind', or 'payload.webhook_url'"})),
            );
        }
    } else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing 'prompt' or 'kind'"})),
        );
    };

    let req = CreateScheduleRequest {
        id: api_req
            .id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        label: api_req.name,
        cron: api_req.cron,
        timezone: api_req.timezone.unwrap_or_else(|| "UTC".to_string()),
        kind,
    };

    match scheduler.create_task(req).await {
        Ok(task) => (StatusCode::CREATED, Json(json!(task))),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

/// `DELETE /api/v1/schedules/:id` — remove a scheduled task.
async fn delete_schedule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    match scheduler.delete_task(&id).await {
        Ok(()) => (StatusCode::OK, Json(json!({"deleted": id}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))),
    }
}

/// `PUT /api/v1/schedules/:id` — update an existing scheduled task.
async fn update_schedule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    result: Result<Json<ApiUpdateScheduleRequest>, JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    let Json(api_req) = match result {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
        }
    };

    let kind = api_req.prompt.map(|p| TaskKind::AgentPrompt { prompt: p });

    let req = UpdateScheduleRequest {
        label: api_req.name,
        cron: api_req.cron,
        timezone: api_req.timezone,
        kind,
    };

    match scheduler.update_task(&id, req).await {
        Ok(task) => (StatusCode::OK, Json(json!(task))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))),
    }
}

#[derive(Debug, serde::Deserialize)]
struct ApiUpdateScheduleRequest {
    #[serde(alias = "label")]
    name: Option<String>,
    cron: Option<String>,
    timezone: Option<String>,
    prompt: Option<String>,
}

/// `POST /api/v1/schedules/:id/pause` — pause a scheduled task.
async fn pause_schedule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    match scheduler.pause_task(&id).await {
        Ok(()) => (StatusCode::OK, Json(json!({"paused": id}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))),
    }
}

/// `POST /api/v1/schedules/:id/resume` — resume a paused task.
async fn resume_schedule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    match scheduler.resume_task(&id).await {
        Ok(()) => (StatusCode::OK, Json(json!({"resumed": id}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))),
    }
}

/// `POST /api/v1/schedules/:id/run-now` — fire a task immediately.
async fn run_schedule_now(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    match scheduler.run_now(&id).await {
        Ok(()) => (StatusCode::ACCEPTED, Json(json!({"fired": id}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))),
    }
}

/// `GET /api/v1/schedules/:id/runs` — execution history for a schedule.
async fn list_schedule_runs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    let limit: u32 = params
        .get("limit")
        .and_then(|v: &String| v.parse().ok())
        .unwrap_or(10);
    match scheduler.get_runs(&id, limit).await {
        Ok(runs) => (StatusCode::OK, Json(json!(runs))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

/// `GET /api/v1/schedules/upcoming` — active schedules sorted by next fire.
async fn list_upcoming_schedules(
    State(state): State<Arc<AppState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    let limit: u32 = params
        .get("limit")
        .and_then(|v: &String| v.parse().ok())
        .unwrap_or(10);
    match scheduler.list_upcoming(limit).await {
        Ok(tasks) => (StatusCode::OK, Json(json!(tasks))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

/// `GET /api/v1/schedules/events` — SSE stream of schedule completion events.
///
/// The desktop subscribes to this on load to receive real-time notifications
/// when scheduled tasks complete (or fail).
async fn schedule_events_sse(
    State(state): State<Arc<AppState>>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.schedule_result_tx.subscribe();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let data = serde_json::to_string(&event).unwrap_or_default();
                    yield Ok(Event::default().data(data));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("schedule events SSE lagged by {n} messages");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(serde::Deserialize)]
struct NotificationStreamParams {
    device_id: Option<String>,
}

/// `GET /api/v1/notifications/stream?device_id=X` — foreground push (#99).
///
/// A paired device opens this to receive notifications in real time. On connect
/// it first drains anything queued while it was offline, then tails live events
/// addressed to it (or `"broadcast"`). Notifications carry a stable `id`; clients
/// dedupe by it. Bounded by `notification_sse_semaphore`.
async fn notifications_stream(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<NotificationStreamParams>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)>
{
    let device_id = params.device_id.filter(|s| !s.trim().is_empty()).ok_or((
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "`device_id` query param required" })),
    ))?;

    let Some(queue) = state.notification_queue.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "notifications not available" })),
        ));
    };

    // The stream must belong to a known, registered device.
    let exists = state
        .device_registry
        .get_device(&device_id)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "notifications: device lookup failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "device lookup failed" })),
            )
        })?;
    if exists.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown device" })),
        ));
    }

    // Bound concurrent notification streams. Deliberately NOT the chat
    // `sse_semaphore`: these connections are long-lived (a phone holds one open
    // indefinitely) and must never starve interactive chat streaming.
    let permit = state
        .notification_sse_semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "Too many concurrent streams" })),
            )
        })?;

    let mut rx = state.notification_tx.subscribe();

    let stream = async_stream::stream! {
        let _permit = permit; // held for the stream's lifetime

        // 1. Flush notifications queued while the device was offline.
        match queue.list_undelivered(&device_id).await {
            Ok(pending) => {
                let mut ids = Vec::with_capacity(pending.len());
                for n in &pending {
                    let data = serde_json::to_string(n).unwrap_or_default();
                    ids.push(n.id.clone());
                    yield Ok(Event::default().data(data));
                }
                if !ids.is_empty() {
                    if let Err(e) = queue.mark_delivered(&ids).await {
                        tracing::warn!(error = %e, "notifications: flush mark_delivered failed");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "notifications: flush query failed"),
        }

        // 2. Live tail — events for this device or broadcasts.
        loop {
            match rx.recv().await {
                Ok(n) => {
                    if n.target == device_id || n.target == "broadcast" {
                        let targeted = n.target == device_id;
                        let id = n.id.clone();
                        let data = serde_json::to_string(&n).unwrap_or_default();
                        yield Ok(Event::default().data(data));
                        if targeted {
                            if let Err(e) = queue.mark_delivered(&[id]).await {
                                tracing::warn!(error = %e, "notifications: live mark_delivered failed");
                            }
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(k)) => {
                    tracing::debug!("notifications SSE lagged by {k} messages");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// ── Agent tools handler ───────────────────────────────────────────────────────

/// Map of tool name prefixes to their MCP App resource URIs.
/// When a tool's fully-qualified name starts with one of these prefixes,
/// the `_meta.ui.resourceUri` field is injected in the tool listing.
const TOOL_UI_RESOURCES: &[(&str, &str)] = &[(
    "giap-weather__get_current_weather",
    "ui://giap-weather/weather-card.html",
)];

/// `GET /api/v1/agent/tools` — list all MCP tools currently loaded by the agent.
///
/// Returns a flat array of tool objects. Each entry includes at minimum
/// `{ "name": "..." }`. Tools with associated MCP App resources also
/// include `{ "_meta": { "ui": { "resourceUri": "ui://..." } } }`.
/// Returns an empty array when no extension manager is active (no-crash fallback).
async fn list_agent_tools(State(state): State<Arc<AppState>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(manager) = &state.extension_manager else {
        return Json(json!([])).into_response();
    };
    match manager.list_tools().await {
        Ok(tools) => {
            let enriched: Vec<Value> = tools
                .iter()
                .map(|tool_name| {
                    let mut obj = json!({ "name": tool_name });
                    // Inject _meta.ui for tools that have an associated MCP App
                    for &(prefix, uri) in TOOL_UI_RESOURCES {
                        if tool_name == prefix || tool_name.ends_with(prefix) {
                            obj["_meta"] = json!({
                                "ui": { "resourceUri": uri }
                            });
                            break;
                        }
                    }
                    obj
                })
                .collect();
            Json(json!(enriched)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ── MCP App Resource handlers ────────────────────────────────────────────────

/// Query parameters for `GET /api/v1/mcp/resources`.
#[derive(Debug, Deserialize)]
struct McpResourceQuery {
    /// The `ui://` resource URI to read.
    uri: String,
}

/// `GET /api/v1/mcp/resources?uri=ui://giap-weather/weather-card.html`
///
/// Proxies MCP resource reads. Looks up the URI in the static resource
/// registry (populated at startup from embedded HTML files) and returns
/// the content in MCP `ReadResourceResult` format.
///
/// Response: `{ "contents": [{ "uri": "...", "text": "..." }] }`
async fn mcp_read_resource(
    State(state): State<Arc<AppState>>,
    Query(query): Query<McpResourceQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let uri = query.uri.trim();
    if uri.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing 'uri' query parameter" })),
        )
            .into_response();
    }

    match state.mcp_app_resources.get(uri) {
        Some(html) => Json(json!({
            "contents": [{
                "uri": uri,
                "text": html,
                "mimeType": "text/html"
            }]
        }))
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("Resource not found: {}", uri) })),
        )
            .into_response(),
    }
}

/// Request body for `POST /api/v1/tools/invoke`.
#[derive(Debug, Deserialize)]
struct InvokeToolRequest {
    /// MCP server prefix (e.g. "giap-device-control"). Optional when `tool` is
    /// already fully-qualified (contains "__").
    #[serde(default)]
    server: String,
    /// Tool name — bare (e.g. "set_device_state") or fully-qualified.
    tool: String,
    /// Tool arguments (JSON object). Defaults to `{}`.
    #[serde(default)]
    args: Value,
}

/// Request body for `POST /api/v1/mcp/tools/call`.
#[derive(Debug, Deserialize)]
struct McpToolCallRequest {
    /// Fully-qualified tool name (e.g. "giap-weather__get_current_weather").
    name: String,
    /// Tool arguments (JSON object).
    arguments: Option<Value>,
}

/// Compose the fully-qualified tool name the dispatcher expects
/// (`"<server>__<tool>"`), unless `tool` is already qualified or no server given.
fn qualify_tool_name(server: &str, tool: &str) -> String {
    if tool.contains("__") || server.is_empty() {
        tool.to_string()
    } else {
        format!("{server}__{tool}")
    }
}

/// Shared direct-dispatch path for the tool-invoke endpoints. Bypasses the LLM:
/// routes straight to the MCP tool registry and returns the raw result.
async fn dispatch_tool_direct(
    state: &Arc<AppState>,
    qualified: &str,
    args: Value,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let dispatcher = state.tool_dispatcher.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "Tool dispatch is not available on this server." })),
        )
    })?;
    let args = if args.is_null() { json!({}) } else { args };
    match dispatcher.dispatch(qualified, args).await {
        Ok(result) => Ok(Json(json!({
            "tool": qualified,
            "success": result.success,
            "content": result.content,
        }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Tool dispatch failed: {e}") })),
        )),
    }
}

/// `POST /api/v1/tools/invoke` — run an MCP tool directly, bypassing the LLM.
///
/// Lets the desktop Hub actuate devices (and call any builtin tool) without a
/// chat turn. Body: `{ "server": "...", "tool": "...", "args": { ... } }`.
async fn invoke_tool(
    State(state): State<Arc<AppState>>,
    body: Result<Json<InvokeToolRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Invalid request: {e}") })),
        )
    })?;
    if req.tool.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "`tool` is required." })),
        ));
    }
    let qualified = qualify_tool_name(&req.server, &req.tool);
    dispatch_tool_direct(&state, &qualified, req.args).await
}

/// `POST /api/v1/mcp/tools/call` — execute an MCP tool directly by qualified name.
///
/// Used by MCP Apps (`app.callServerTool()`). Delegates to the same dispatcher
/// as `/tools/invoke`.
async fn mcp_call_tool(
    State(state): State<Arc<AppState>>,
    body: Result<Json<McpToolCallRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Invalid request: {e}") })),
        )
    })?;
    dispatch_tool_direct(
        &state,
        &req.name,
        req.arguments.unwrap_or_else(|| json!({})),
    )
    .await
}

// ── Agent chat stream ─────────────────────────────────────────────────────────

/// `POST /api/v1/agent/chat/stream` — run a full agentic loop (Goose + MCP tools)
/// and stream the result via SSE.
///
/// Unlike `/chat/stream` (which uses the LlmProvider directly), this handler
/// runs the GooseAdapter's agentic loop which can call MCP tools, multi-step
/// reasoning, etc. while keeping the SSE connection alive with status events.
///
/// Event shapes:
/// - `{"type":"status","content":"Agent working…"}` — heartbeat while loop runs
/// - `{"type":"tool_call","tool":"<id>"}` — each MCP tool that was invoked
/// - `{"type":"text","content":"...","token":"..."}` — final response text
/// - `{"done":true,"session_id":"..."}` — completion
/// - `{"error":"..."}` — on failure
async fn agent_chat_stream(
    State(state): State<Arc<AppState>>,
    body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use futures::stream::StreamExt;
    use pond_core::models::ports::agent::AgentStreamEvent;
    use pond_core::shared::domain::agent::AgentRequest;

    // Update activity timestamp — resets the consolidation inactivity timer
    *state.last_user_activity.write().await = std::time::Instant::now();
    // Cancel any in-progress consolidation
    if let Some(cancel) = state.consolidation_cancel.read().await.as_ref() {
        cancel.cancel();
    }

    let permit = match state.sse_semaphore.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "Too many concurrent streams"})),
            )
                .into_response();
        }
    };

    let body = match body {
        Ok(b) => b.0,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let message = body["message"].as_str().unwrap_or("").to_string();
    let session_id = body["session_id"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let agent = state.agent.clone();
    let storage = state.session_storage.clone();

    let stream = async_stream::stream! {
        let _permit = permit;

        let chat_service = pond_core::shared::services::chat::ChatService::new(
            agent.clone(),
            session_id.clone(),
            storage.clone(),
        );

        if storage.get_session(&session_id).await.is_err() {
            if let Err(e) = storage.create_session(session_id.clone()).await {
                yield Ok(Event::default().data(json!({"error": e.to_string()}).to_string()));
                return;
            }
        }

        if let Err(e) = chat_service.persist_user_message(&message).await {
            yield Ok(Event::default().data(json!({"error": format!("Failed to persist user message: {}", e)}).to_string()));
            return;
        }

        let mut full_text = String::new();
        let mut tool_results: Vec<String> = Vec::new();

        let request = AgentRequest {
            message,
            session_id: session_id.clone(),
            model_role: "task".to_string(),
            images: Vec::new(),
            voice_mode: false,
            canvas_mode: false,
        };

        let mut agent_stream = match agent.chat_stream(request).await {
            Ok(s) => s,
            Err(e) => {
                let data = json!({"error": e.to_string()}).to_string();
                yield Ok(Event::default().data(data));
                return;
            }
        };

        // See chat_stream above for rationale — same Harmony preamble filter.
        let mut thought = crate::thought_filter::ThoughtFilter::new();

        while let Some(event_result) = agent_stream.next().await {
            match event_result {
                Ok(event) => {
                    let maybe_data = match event {
                        AgentStreamEvent::Status { content } => {
                            Some(json!({"type": "status", "content": content}).to_string())
                        }
                        AgentStreamEvent::Thinking { content } => {
                            Some(json!({"type": "thinking", "content": content}).to_string())
                        }
                        AgentStreamEvent::ToolCall { tool, id, input } => {
                            Some(json!({"type": "tool_call", "tool": tool, "id": id, "input": input}).to_string())
                        }
                        AgentStreamEvent::ToolResult { tool, id, content } => {
                            let (clean_content, ui_hint) = extract_ui_hint(&content);
                            tool_results.push(json!({
                                "tool_call_id": id,
                                "tool": tool,
                                "content": clean_content,
                            }).to_string());
                            let mut ev = serde_json::json!({
                                "type": "tool_result",
                                "tool": tool,
                                "id": id,
                                "content": clean_content
                            });
                            if let Some(ui) = ui_hint {
                                ev["ui"] = ui;
                            }
                            Some(ev.to_string())
                        }
                        AgentStreamEvent::Text { content } => {
                            let visible = thought.push(&content);
                            if visible.is_empty() {
                                None
                            } else {
                                full_text.push_str(&visible);
                                Some(json!({"type": "text", "content": visible, "token": visible}).to_string())
                            }
                        }
                        AgentStreamEvent::ReviewStatus { content } => {
                            Some(json!({"type": "review_status", "content": content}).to_string())
                        }
                        AgentStreamEvent::ReviewRevision { content, score, rounds } => {
                            Some(json!({"type": "review_revision", "content": content, "score": score, "rounds": rounds}).to_string())
                        }
                        AgentStreamEvent::Done { .. } => {
                            Some(json!({"done": true, "session_id": session_id.clone()}).to_string())
                        }
                        AgentStreamEvent::Error { content } => {
                            Some(json!({"error": content}).to_string())
                        }
                    };
                    if let Some(data) = maybe_data {
                        yield Ok::<Event, std::convert::Infallible>(Event::default().data(data));
                    }
                    // Emit thinking blocks captured by the filter
                    for thinking_content in thought.take_thinking() {
                        let data = json!({"type": "thinking", "content": thinking_content}).to_string();
                        yield Ok::<Event, std::convert::Infallible>(Event::default().data(data));
                    }
                    // Execute Harmony-format tool-call envelopes captured by ThoughtFilter.
                    for body in thought.take_tool_calls() {
                        if let Some((name, args)) = crate::thought_filter::parse_tool_envelope(&body) {
                            tracing::info!(tool = %name, "Executing text-based tool call (agent chat)");
                            let call_id = uuid::Uuid::new_v4().to_string();
                            let args_val: serde_json::Value = serde_json::from_str(&args).unwrap_or(json!({}));
                            yield Ok::<Event, std::convert::Infallible>(Event::default().data(
                                json!({"type": "tool_call", "tool": name.clone(), "id": call_id.clone(), "input": args_val}).to_string()
                            ));
                            match state.agent.call_tool(&session_id, &name, &args).await {
                                Ok(result_text) => {
                                    let (clean, ui_hint) = extract_ui_hint(&result_text);
                                    let mut ev = json!({"type": "tool_result", "tool": name, "id": call_id, "content": clean});
                                    if let Some(ui) = ui_hint {
                                        ev["ui"] = ui;
                                    }
                                    yield Ok::<Event, std::convert::Infallible>(Event::default().data(ev.to_string()));
                                }
                                Err(e) => {
                                    yield Ok::<Event, std::convert::Infallible>(Event::default().data(
                                        json!({"type": "tool_result", "tool": name, "id": call_id, "content": format!("Tool error: {e}")}).to_string()
                                    ));
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    let data = json!({"error": e.to_string()}).to_string();
                    yield Ok(Event::default().data(data));
                    return;
                }
            }
        }

        let tail = thought.flush();
        if !tail.is_empty() {
            full_text.push_str(&tail);
            let data = json!({"type": "text", "content": tail, "token": tail}).to_string();
            yield Ok::<Event, std::convert::Infallible>(Event::default().data(data));
        }

        // ── Persist assistant turn ──────────────────────────────────────────
        let _ = chat_service.persist_assistant_turn(
            tool_results,
            &full_text,
            None,
            None,
        ).await;
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

// ── Event log / telemetry ─────────────────────────────────────────────────────

/// `GET /api/v1/logs` — return recent entries from the `event_log` table.
///
/// Query params:
/// - `limit` (default 200, max 2000)
/// - `level` — filter to `INFO`, `WARN`, or `ERROR`
async fn list_logs(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(repo) = &state.event_log_repo else {
        return Json(json!([])).into_response();
    };

    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(200)
        .min(2000);
    let level = params.get("level").map(|s| s.as_str());

    match repo.list(limit, level).await {
        Ok(entries) => Json(json!(entries)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// `GET /api/v1/logs/export` — download the event log as a CSV file.
///
/// Returns up to 10,000 rows across all severity levels.
async fn export_logs_csv(State(state): State<Arc<AppState>>) -> axum::response::Response {
    use axum::response::IntoResponse;

    let Some(repo) = &state.event_log_repo else {
        return (StatusCode::NOT_IMPLEMENTED, "Event log not configured").into_response();
    };

    let entries = match repo.list(10_000, None).await {
        Ok(e) => e,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    let mut csv = "id,timestamp,level,source,message,metadata\n".to_string();
    for entry in &entries {
        fn esc(s: &str) -> String {
            if s.contains(',') || s.contains('"') || s.contains('\n') {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.to_string()
            }
        }
        csv.push_str(&format!(
            "{},{},{},{},{},{}\n",
            entry.id,
            esc(&entry.timestamp),
            esc(&entry.level),
            esc(&entry.source),
            esc(&entry.message),
            esc(entry.metadata.as_deref().unwrap_or("")),
        ));
    }

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/csv; charset=utf-8")
        .header(
            "Content-Disposition",
            "attachment; filename=\"pond-logs.csv\"",
        )
        .body(Body::from(csv))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ── Extension management handlers ─────────────────────────────────────────────

/// `GET /api/v1/extensions` — list all active Goose/MCP extensions.
async fn list_extensions_handler(State(state): State<Arc<AppState>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(manager) = &state.extension_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Extension manager not available"})),
        )
            .into_response();
    };
    match manager.list_extensions().await {
        Ok(mut exts) => {
            // Merge in persisted-but-disabled extensions so the UI sees them
            if let Some(repo) = &state.mcp_server_repo {
                if let Ok(persisted) = repo.list().await {
                    let live_names: std::collections::HashSet<String> =
                        exts.iter().map(|e| e.name.clone()).collect();
                    for srv in persisted {
                        if !live_names.contains(&srv.name) {
                            // This extension is persisted but not live — disabled or failed
                            exts.push(ExtensionInfo {
                                name: srv.name,
                                kind: srv.kind,
                                description: srv.description,
                                tools: vec![],
                                enabled: srv.enabled,
                                status: if srv.enabled {
                                    "error".to_string()
                                } else {
                                    "disabled".to_string()
                                },
                                last_error: if srv.enabled {
                                    Some("Extension failed to load".to_string())
                                } else {
                                    None
                                },
                            });
                        }
                    }
                }
            }
            Json(json!({"extensions": exts})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// `POST /api/v1/extensions` — register a new MCP extension and persist it.
async fn add_extension_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<pond_core::mcp::ports::extension_manager::AddExtensionRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(manager) = &state.extension_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Extension manager not available"})),
        )
            .into_response();
    };
    match manager.add_extension(req.clone()).await {
        Ok(info) => {
            // Persist so the server reconnects on restart.
            if let Some(repo) = &state.mcp_server_repo {
                let cfg = pond_core::mcp::ports::mcp_server::McpServerConfig {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: req.name.clone(),
                    kind: req.kind.clone(),
                    description: req.description.clone(),
                    command: req.command.clone(),
                    args: req.args.clone(),
                    env: req.env.clone(),
                    uri: req.uri.clone(),
                    enabled: true,
                    created_at: chrono::Utc::now().to_rfc3339(),
                };
                if let Err(e) = repo.save(&cfg).await {
                    tracing::warn!("Failed to persist MCP server '{}': {e}", req.name);
                }
            }
            // Sync discovered tools into the tool registry
            if let Some(registry) = &state.tool_registry {
                if let Ok(all_tools) = manager.list_tools().await {
                    let prefix = format!("{}__", req.name);
                    let ext_tools: Vec<(String, String)> = all_tools
                        .iter()
                        .filter(|t| t.starts_with(&prefix))
                        .map(|t| {
                            let tool_name = t.strip_prefix(&prefix).unwrap_or(t).to_string();
                            (tool_name, String::new())
                        })
                        .collect();
                    if !ext_tools.is_empty() {
                        registry
                            .register_extension_tools(&req.name, ext_tools)
                            .await;
                    }
                }
            }
            (StatusCode::CREATED, Json(info)).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// `DELETE /api/v1/extensions/:name` — remove a registered extension and its persisted config.
async fn remove_extension_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(manager) = &state.extension_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Extension manager not available"})),
        )
            .into_response();
    };
    match manager.remove_extension(&name).await {
        Ok(()) => {
            if let Some(repo) = &state.mcp_server_repo {
                if let Err(e) = repo.delete(&name).await {
                    tracing::warn!("Failed to remove persisted MCP server '{name}': {e}");
                }
            }
            // Remove extension tools from the registry
            if let Some(registry) = &state.tool_registry {
                registry.deregister_extension(&name).await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

/// `PATCH /api/v1/extensions/{name}` — enable or disable an extension.
///
/// Body: `{"enabled": true}` or `{"enabled": false}`.
/// Disabled extensions are excluded from future Goose agent sessions.
async fn toggle_extension_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(manager) = &state.extension_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Extension manager not available"})),
        )
            .into_response();
    };

    let body = match body {
        Ok(b) => b.0,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let enabled = body["enabled"].as_bool().unwrap_or(true);

    match manager.set_enabled(&name, enabled).await {
        Ok(()) => {
            if let Some(repo) = &state.mcp_server_repo {
                if let Err(e) = repo.set_enabled(&name, enabled).await {
                    tracing::warn!("Failed to persist enabled state for '{}': {e}", name);
                }
            }
            // Sync tool registry: re-register on enable, deregister on disable
            if let Some(registry) = &state.tool_registry {
                if enabled {
                    if let Ok(all_tools) = manager.list_tools().await {
                        let prefix = format!("{}__", name);
                        let ext_tools: Vec<(String, String)> = all_tools
                            .iter()
                            .filter(|t| t.starts_with(&prefix))
                            .map(|t| {
                                let tool_name = t.strip_prefix(&prefix).unwrap_or(t).to_string();
                                (tool_name, String::new())
                            })
                            .collect();
                        if !ext_tools.is_empty() {
                            registry.register_extension_tools(&name, ext_tools).await;
                        }
                    }
                } else {
                    registry.deregister_extension(&name).await;
                }
            }
            Json(json!({"name": name, "enabled": enabled})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── Extension Marketplace ────────────────────────────────────────────────────

/// `GET /api/v1/marketplace` — list available extensions from the curated registry.
async fn list_marketplace_handler(State(state): State<Arc<AppState>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(mp) = &state.marketplace else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Marketplace not available"})),
        )
            .into_response();
    };
    match mp.list_available().await {
        Ok(exts) => Json(json!({"extensions": exts})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// `POST /api/v1/marketplace/{id}/install` — install a marketplace extension.
///
/// Looks up the extension by ID in the marketplace catalogue, converts it to
/// an `AddExtensionRequest`, and delegates to the extension manager. The
/// server configuration is persisted so the extension reconnects on restart.
///
/// Accepts an optional JSON body `{ "secrets": { "KEY": "value", ... } }` to
/// supply required secrets at install time. Missing required secrets cause a
/// 428 Precondition Required response listing what is needed.
async fn install_marketplace_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(mp) = &state.marketplace else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Marketplace not available"})),
        )
            .into_response();
    };
    let Some(manager) = &state.extension_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Extension manager not available"})),
        )
            .into_response();
    };

    let ext = match mp.get_by_id(&id).await {
        Ok(Some(e)) => e,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("Extension '{}' not found", id)})),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    // Extract secrets from the optional request body.
    let secrets: std::collections::HashMap<String, String> = body
        .and_then(|b| {
            b.0.get("secrets")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
        })
        .unwrap_or_default();

    // Check that all required (non-OAuth) secrets are provided or already stored.
    if !ext.required_secrets.is_empty() {
        let mut missing = Vec::new();
        for sr in &ext.required_secrets {
            if !sr.required || sr.kind == pond_core::security::domain::secret::SecretKind::OAuthFlow
            {
                continue;
            }
            if secrets.contains_key(&sr.key) {
                continue;
            }
            // Check if already stored in the secret repository.
            let already_stored = if let Some(repo) = &state.secret_repo {
                repo.has(&sr.key).await.unwrap_or(false)
            } else {
                false
            };
            if !already_stored {
                missing.push(sr);
            }
        }
        if !missing.is_empty() {
            return (
                StatusCode::PRECONDITION_REQUIRED,
                Json(json!({
                    "error": "Missing required secrets",
                    "missing": missing.iter().map(|s| json!({
                        "key": s.key,
                        "display_name": s.display_name,
                        "description": s.description,
                        "kind": s.kind,
                    })).collect::<Vec<_>>()
                })),
            )
                .into_response();
        }
    }

    // Store any provided secrets.
    if let Some(repo) = &state.secret_repo {
        for (key, value) in &secrets {
            if let Err(e) = repo.set(key, value).await {
                tracing::warn!("Failed to store secret '{}': {e}", key);
            }
        }
    }

    // Build the env map: start with provided secrets, then fill in any
    // already-stored secrets that weren't explicitly provided.
    let mut env = secrets;
    if let Some(repo) = &state.secret_repo {
        for sr in &ext.required_secrets {
            if !env.contains_key(&sr.key) {
                if let Ok(Some(val)) = repo.get(&sr.key).await {
                    env.insert(sr.key.clone(), val);
                }
            }
        }
    }
    env.insert(
        crate::oauth_callback::INTERNAL_TOKEN_ENV_KEY.to_string(),
        crate::oauth_callback::internal_extension_token().to_string(),
    );

    let req = pond_core::mcp::ports::extension_manager::AddExtensionRequest {
        name: ext.id.clone(),
        kind: ext.kind.clone(),
        description: ext.description.clone(),
        command: ext.command.clone(),
        args: ext.args.clone(),
        env: env.clone(),
        uri: ext.uri.clone(),
    };

    match manager.add_extension(req.clone()).await {
        Ok(info) => {
            // Persist so the extension reconnects on restart
            if let Some(repo) = &state.mcp_server_repo {
                let cfg = pond_core::mcp::ports::mcp_server::McpServerConfig {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: ext.id.clone(),
                    kind: ext.kind,
                    description: ext.description,
                    command: ext.command,
                    args: ext.args,
                    env,
                    uri: ext.uri,
                    enabled: true,
                    created_at: chrono::Utc::now().to_rfc3339(),
                };
                if let Err(e) = repo.save(&cfg).await {
                    tracing::warn!("Failed to persist marketplace extension '{}': {e}", ext.id);
                }
            }
            // Sync tool registry
            if let Some(registry) = &state.tool_registry {
                if let Ok(all_tools) = manager.list_tools().await {
                    let prefix = format!("{}__", req.name);
                    let ext_tools: Vec<(String, String)> = all_tools
                        .iter()
                        .filter(|t| t.starts_with(&prefix))
                        .map(|t| {
                            let tool_name = t.strip_prefix(&prefix).unwrap_or(t).to_string();
                            (tool_name, String::new())
                        })
                        .collect();
                    if !ext_tools.is_empty() {
                        registry
                            .register_extension_tools(&req.name, ext_tools)
                            .await;
                    }
                }
            }
            (StatusCode::CREATED, Json(json!(info))).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── Secret management handlers ────────────────────────────────────────────────

/// `GET /api/v1/secrets` — list stored secret key names (never values).
async fn list_secrets_handler(State(state): State<Arc<AppState>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(repo) = &state.secret_repo else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Secret storage not available"})),
        )
            .into_response();
    };
    match repo.list_keys().await {
        Ok(keys) => Json(json!({"keys": keys})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// `GET /api/v1/secrets/{key}/exists` — check if a secret is set.
async fn check_secret_handler(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(repo) = &state.secret_repo else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Secret storage not available"})),
        )
            .into_response();
    };
    match repo.has(&key).await {
        Ok(exists) => Json(json!({"key": key, "exists": exists})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// `PUT /api/v1/secrets/{key}` — set a secret value.
///
/// Body: `{ "value": "secret-value-here" }`
async fn set_secret_handler(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(repo) = &state.secret_repo else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Secret storage not available"})),
        )
            .into_response();
    };
    let Some(value) = body["value"].as_str() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Missing 'value' field"})),
        )
            .into_response();
    };
    match repo.set(&key, value).await {
        Ok(()) => Json(json!({"key": key, "stored": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// `DELETE /api/v1/secrets/{key}` — delete a secret.
async fn delete_secret_handler(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(repo) = &state.secret_repo else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Secret storage not available"})),
        )
            .into_response();
    };
    match repo.delete(&key).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// `GET /api/v1/extensions/{name}/secrets` — get secret requirements and fulfillment status.
///
/// Looks up the extension by name in the marketplace registry, returns its
/// `required_secrets` along with a `fulfilled` map indicating which keys are
/// already stored in the secret repository.
async fn get_extension_secrets_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(mp) = &state.marketplace else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Marketplace not available"})),
        )
            .into_response();
    };

    let ext = match mp.get_by_id(&name).await {
        Ok(Some(e)) => e,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Extension not found"})),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };

    let mut fulfilled = std::collections::HashMap::new();
    if let Some(repo) = &state.secret_repo {
        for sr in &ext.required_secrets {
            fulfilled.insert(sr.key.clone(), repo.has(&sr.key).await.unwrap_or(false));
        }
    }

    Json(json!({
        "requirements": ext.required_secrets,
        "fulfilled": fulfilled,
    }))
    .into_response()
}

/// `POST /api/v1/extensions/{name}/secrets` — set secrets for an extension in bulk.
///
/// Accepts a JSON object `{ "KEY": "value", ... }` and stores each entry in the
/// secret repository. The extension name is used for validation (must exist in
/// the marketplace) but secrets are stored globally by key name.
async fn set_extension_secrets_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(repo) = &state.secret_repo else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Secret storage not available"})),
        )
            .into_response();
    };

    // Validate the extension exists in the marketplace.
    if let Some(mp) = &state.marketplace {
        match mp.get_by_id(&name).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "Extension not found"})),
                )
                    .into_response()
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response()
            }
        }
    }

    let secrets: std::collections::HashMap<String, String> = match serde_json::from_value(body) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };

    for (key, value) in &secrets {
        if let Err(e) = repo.set(key, value).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to store '{}': {}", key, e)})),
            )
                .into_response();
        }
    }

    Json(json!({"stored": secrets.len()})).into_response()
}

// ── OAuth PKCE handlers ──────────────────────────────────────────────────────

/// `POST /api/v1/oauth/authorize` — Start an OAuth PKCE authorization flow.
///
/// Body: `{ "provider": "spotify", "extension_id": "music" }`
///
/// Returns `{ "auth_url": "https://...", "state": "nonce" }`.
/// The client should open `auth_url` in the user's default browser.
async fn oauth_authorize_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    use crate::oauth_callback;
    use axum::response::IntoResponse;

    let provider_id = body["provider"].as_str().unwrap_or("").to_string();
    let extension_id = body["extension_id"].as_str().map(String::from);

    // Find provider config by ID or by token_key (frontend may send the secret key name)
    let providers = pond_core::user_data::services::oauth_providers::builtin_oauth_providers();
    let provider = match providers
        .iter()
        .find(|p| p.id == provider_id || p.token_key == provider_id)
    {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("Unknown OAuth provider: {}", provider_id)})),
            )
                .into_response()
        }
    };

    // Check if user has their own client ID in the secret store
    let client_id = if let Some(repo) = &state.secret_repo {
        let key = format!("{}_CLIENT_ID", provider_id.to_uppercase());
        repo.get(&key)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| provider.bundled_client_id.clone())
    } else {
        provider.bundled_client_id.clone()
    };

    let (code_verifier, code_challenge) = oauth_callback::generate_pkce();
    let state_nonce = oauth_callback::generate_state();

    // Store PKCE session
    {
        let mut sessions = state.oauth_state.write().await;
        sessions.insert(
            state_nonce.clone(),
            oauth_callback::PkceSession {
                provider_id: provider.id.clone(),
                code_verifier,
                extension_id,
                created_at: std::time::Instant::now(),
            },
        );
    }

    let redirect_uri = format!("http://127.0.0.1:{}/api/v1/oauth/callback", state.api_port);
    let scopes = provider.scopes.join(" ");

    let auth_url = format!(
        "{}?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        provider.authorize_url,
        urlencoding::encode(&client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&scopes),
        urlencoding::encode(&state_nonce),
        urlencoding::encode(&code_challenge),
    );

    tracing::info!(
        provider = %provider_id,
        redirect_uri = %redirect_uri,
        "OAuth authorize URL: {}",
        auth_url
    );

    Json(json!({"auth_url": auth_url, "state": state_nonce})).into_response()
}

/// `GET /api/v1/oauth/callback` — Handle the OAuth provider's redirect.
///
/// Query: `?code=...&state=...`
///
/// Exchanges the authorization code for tokens using the stored PKCE
/// verifier, persists access/refresh tokens in the secret store, and
/// returns a success HTML page the user can close.
async fn oauth_callback_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::{Html, IntoResponse};

    let code = params.get("code").cloned().unwrap_or_default();
    let state_nonce = params.get("state").cloned().unwrap_or_default();

    // Look up and consume the PKCE session
    let session = {
        let mut sessions = state.oauth_state.write().await;
        sessions.remove(&state_nonce)
    };

    let session = match session {
        Some(s) => s,
        None => {
            return Html(
                "<h1>Authorization failed</h1>\
                 <p>Invalid or expired state. Please try again.</p>"
                    .to_string(),
            )
            .into_response()
        }
    };

    // Find provider config
    let providers = pond_core::user_data::services::oauth_providers::builtin_oauth_providers();
    let provider = match providers.iter().find(|p| p.id == session.provider_id) {
        Some(p) => p,
        None => {
            return Html("<h1>Authorization failed</h1><p>Unknown provider.</p>".to_string())
                .into_response()
        }
    };

    // Resolve client ID (user override or bundled)
    let client_id = if let Some(repo) = &state.secret_repo {
        let key = format!("{}_CLIENT_ID", session.provider_id.to_uppercase());
        repo.get(&key)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| provider.bundled_client_id.clone())
    } else {
        provider.bundled_client_id.clone()
    };

    // Exchange authorization code for tokens.
    // The redirect_uri MUST exactly match the one sent in the authorize request.
    let redirect_uri = format!("http://127.0.0.1:{}/api/v1/oauth/callback", state.api_port);
    let token_response = state
        .http_client
        .post(&provider.token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", client_id.as_str()),
            ("code_verifier", session.code_verifier.as_str()),
        ])
        .send()
        .await;

    match token_response {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();

            // Store tokens in the secret repository
            if let Some(repo) = &state.secret_repo {
                if let Some(access_token) = body["access_token"].as_str() {
                    let _ = repo.set(&provider.token_key, access_token).await;
                }
                if let Some(refresh_token) = body["refresh_token"].as_str() {
                    let _ = repo.set(&provider.refresh_key, refresh_token).await;
                }
            }

            tracing::info!(provider = %provider.id, "OAuth token exchange succeeded");

            // If this OAuth flow was triggered by an extension install, restart
            // the extension so the child process picks up the new tokens.
            if let Some(ext_id) = &session.extension_id {
                if let (Some(mgr), Some(mp), Some(secret_repo)) = (
                    &state.extension_manager,
                    &state.marketplace,
                    &state.secret_repo,
                ) {
                    if let Ok(Some(ext)) = mp.get_by_id(ext_id).await {
                        // Build env map with all resolved secrets
                        let mut env = std::collections::HashMap::new();
                        for sr in &ext.required_secrets {
                            if let Ok(Some(val)) = secret_repo.get(&sr.key).await {
                                env.insert(sr.key.clone(), val);
                            }
                        }
                        env.insert(
                            crate::oauth_callback::INTERNAL_TOKEN_ENV_KEY.to_string(),
                            crate::oauth_callback::internal_extension_token().to_string(),
                        );

                        // Remove the running extension and re-add with new env
                        let _ = mgr.remove_extension(ext_id).await;

                        let req = pond_core::mcp::ports::extension_manager::AddExtensionRequest {
                            name: ext.id.clone(),
                            kind: ext.kind.clone(),
                            description: ext.description.clone(),
                            command: ext.command.clone(),
                            args: ext.args.clone(),
                            env,
                            uri: ext.uri.clone(),
                        };

                        match mgr.add_extension(req).await {
                            Ok(_) => tracing::info!(
                                extension = %ext_id,
                                "restarted extension with OAuth tokens"
                            ),
                            Err(e) => tracing::warn!(
                                extension = %ext_id,
                                error = %e,
                                "failed to restart extension after OAuth"
                            ),
                        }
                    }
                }
            }

            Html(format!(
                r#"<!DOCTYPE html>
<html><head><title>Authorization Successful</title>
<style>body{{font-family:system-ui;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#f8f9fa}}
.card{{text-align:center;padding:2rem;border-radius:12px;background:white;box-shadow:0 2px 8px rgba(0,0,0,0.1)}}
h1{{color:#22c55e;margin:0 0 .5rem}}p{{color:#6b7280}}</style></head>
<body><div class="card"><h1>Connected to {}</h1><p>You can close this window and return to Goose in a Pond.</p></div></body></html>"#,
                provider.display_name
            ))
            .into_response()
        }
        Ok(resp) => {
            let error_body = resp.text().await.unwrap_or_default();
            tracing::warn!(provider = %provider.id, error = %error_body, "OAuth token exchange failed");
            Html(format!(
                "<h1>Authorization failed</h1>\
                 <p>Token exchange error. Please try again.</p>\
                 <pre>{}</pre>",
                error_body
            ))
            .into_response()
        }
        Err(e) => {
            tracing::error!(provider = %provider.id, error = %e, "OAuth token exchange network error");
            Html(format!(
                "<h1>Authorization failed</h1><p>Network error: {}</p>",
                e
            ))
            .into_response()
        }
    }
}

/// `POST /api/v1/oauth/refresh` — Refresh an expired OAuth access token.
///
/// Body: `{ "provider": "spotify" }`
///
/// Uses the stored refresh token to obtain a new access token.
async fn oauth_refresh_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    match crate::middleware::extract_bearer_token(&headers) {
        Ok(token) if token == crate::oauth_callback::internal_extension_token() => {}
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Invalid or missing internal token"})),
            )
                .into_response()
        }
    }

    let provider_id = body["provider"].as_str().unwrap_or("").to_string();
    let providers = pond_core::user_data::services::oauth_providers::builtin_oauth_providers();
    let provider = match providers.iter().find(|p| p.id == provider_id) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Unknown provider"})),
            )
                .into_response()
        }
    };

    let Some(repo) = &state.secret_repo else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Secret storage not available"})),
        )
            .into_response();
    };

    let refresh_token = match repo.get(&provider.refresh_key).await {
        Ok(Some(t)) => t,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "No refresh token stored"})),
            )
                .into_response()
        }
    };

    let client_id = {
        let key = format!("{}_CLIENT_ID", provider_id.to_uppercase());
        repo.get(&key)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| provider.bundled_client_id.clone())
    };

    match state
        .http_client
        .post(&provider.token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            if let Some(access_token) = body["access_token"].as_str() {
                let _ = repo.set(&provider.token_key, access_token).await;
            }
            // Some providers rotate refresh tokens
            if let Some(new_refresh) = body["refresh_token"].as_str() {
                let _ = repo.set(&provider.refresh_key, new_refresh).await;
            }
            // Capture the new access token to return in the response.
            let new_access_token = body["access_token"].as_str().map(String::from);
            tracing::info!(provider = %provider_id, "OAuth token refresh succeeded");

            // Spawn extension restart in the background so the HTTP response
            // is sent BEFORE the extension process is killed.  Without this,
            // the calling extension (which triggered the refresh) gets killed
            // before it can read the response containing the new token.
            let token_key = provider.token_key.clone();
            let provider_id_bg = provider_id.clone();
            let mgr = state.extension_manager.clone();
            let mp = state.marketplace.clone();
            let secret_repo = state.secret_repo.clone();
            tokio::spawn(async move {
                // Brief delay so the HTTP response reaches the caller first.
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if let (Some(mgr), Some(mp), Some(secret_repo)) = (mgr, mp, secret_repo) {
                    if let Ok(available) = mp.list_available().await {
                        for ext in available {
                            let uses_token =
                                ext.required_secrets.iter().any(|s| s.key == token_key);
                            if uses_token {
                                let mut env = std::collections::HashMap::new();
                                for sr in &ext.required_secrets {
                                    if let Ok(Some(val)) = secret_repo.get(&sr.key).await {
                                        env.insert(sr.key.clone(), val);
                                    }
                                }
                                env.insert(
                                    crate::oauth_callback::INTERNAL_TOKEN_ENV_KEY.to_string(),
                                    crate::oauth_callback::internal_extension_token().to_string(),
                                );
                                let _ = mgr.remove_extension(&ext.id).await;
                                let req =
                                    pond_core::mcp::ports::extension_manager::AddExtensionRequest {
                                        name: ext.id.clone(),
                                        kind: ext.kind.clone(),
                                        description: ext.description.clone(),
                                        command: ext.command.clone(),
                                        args: ext.args.clone(),
                                        env,
                                        uri: ext.uri.clone(),
                                    };
                                match mgr.add_extension(req).await {
                                    Ok(_) => tracing::info!(
                                        extension = %ext.id,
                                        provider = %provider_id_bg,
                                        "restarted extension with refreshed OAuth tokens"
                                    ),
                                    Err(e) => tracing::warn!(
                                        extension = %ext.id,
                                        provider = %provider_id_bg,
                                        error = %e,
                                        "failed to restart extension after OAuth refresh"
                                    ),
                                }
                            }
                        }
                    }
                }
            });

            let mut resp_body = json!({"refreshed": true});
            if let Some(token) = new_access_token {
                resp_body["access_token"] = serde_json::Value::String(token);
            }
            Json(resp_body).into_response()
        }
        Ok(resp) => {
            let err = resp.text().await.unwrap_or_default();
            tracing::warn!(provider = %provider_id, error = %err, "OAuth token refresh failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("Refresh failed: {}", err)})),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(provider = %provider_id, error = %e, "OAuth refresh network error");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

/// `GET /api/v1/oauth/providers` — List supported OAuth providers.
///
/// Returns `{ "providers": [{ "id": "spotify", "display_name": "Spotify", "scopes": [...] }] }`.
async fn oauth_providers_handler(
    State(_state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let providers = pond_core::user_data::services::oauth_providers::builtin_oauth_providers();
    let list: Vec<serde_json::Value> = providers
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "display_name": p.display_name,
                "scopes": p.scopes,
            })
        })
        .collect();
    Json(json!({"providers": list}))
}

// ── Music (Spotify) ──────────────────────────────────────────────────────────

/// Refreshes the stored Spotify access token using the stored refresh token.
/// Returns the new access token, or `None` if refresh isn't possible.
async fn refresh_spotify_access_token(state: &AppState) -> Option<String> {
    let repo = state.secret_repo.as_ref()?;
    let providers = pond_core::user_data::services::oauth_providers::builtin_oauth_providers();
    let provider = providers.iter().find(|p| p.id == "spotify")?;
    let refresh_token = repo.get(&provider.refresh_key).await.ok().flatten()?;
    let client_id = repo
        .get(&format!("{}_CLIENT_ID", provider.id.to_uppercase()))
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| provider.bundled_client_id.clone());

    let resp = state
        .http_client
        .post(&provider.token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let access_token = body["access_token"].as_str()?.to_string();
    let _ = repo.set(&provider.token_key, &access_token).await;
    if let Some(new_refresh) = body["refresh_token"].as_str() {
        let _ = repo.set(&provider.refresh_key, new_refresh).await;
    }
    Some(access_token)
}

/// Calls the Spotify Web API with the stored access token, transparently
/// refreshing and retrying once on a 401. Returns `None` when there's no
/// token to try at all (Spotify not connected) or refresh fails.
async fn spotify_api_call(
    state: &AppState,
    method: reqwest::Method,
    path: &str,
) -> Option<reqwest::Response> {
    let repo = state.secret_repo.as_ref()?;
    let providers = pond_core::user_data::services::oauth_providers::builtin_oauth_providers();
    let provider = providers.iter().find(|p| p.id == "spotify")?;
    let token = repo.get(&provider.token_key).await.ok().flatten()?;
    let url = format!("https://api.spotify.com/v1{path}");

    let resp = state
        .http_client
        .request(method.clone(), &url)
        .bearer_auth(&token)
        .send()
        .await
        .ok()?;

    if resp.status() != StatusCode::UNAUTHORIZED {
        return Some(resp);
    }

    let refreshed = refresh_spotify_access_token(state).await?;
    state
        .http_client
        .request(method, &url)
        .bearer_auth(&refreshed)
        .send()
        .await
        .ok()
}

/// `GET /api/v1/music/now-playing` — Spotify playback snapshot for the dashboard widget.
async fn music_now_playing_handler(State(state): State<Arc<AppState>>) -> axum::response::Response {
    use axum::response::IntoResponse;

    let Some(resp) =
        spotify_api_call(&state, reqwest::Method::GET, "/me/player/currently-playing").await
    else {
        return Json(json!({"connected": false})).into_response();
    };

    if !resp.status().is_success() {
        // 204 = nothing currently playing; other failures degrade the same way
        // so the widget can just show an idle state either way.
        return Json(json!({"connected": true, "playing": false})).into_response();
    }

    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    let item = &body["item"];

    Json(json!({
        "connected": true,
        "playing": body["is_playing"].as_bool().unwrap_or(false),
        "track": item["name"].as_str().unwrap_or(""),
        "artist": item["artists"][0]["name"].as_str().unwrap_or(""),
        "album_art": item["album"]["images"][0]["url"].as_str(),
        "progress_ms": body["progress_ms"].as_i64().unwrap_or(0),
        "duration_ms": item["duration_ms"].as_i64().unwrap_or(0),
    }))
    .into_response()
}

/// `POST /api/v1/music/control` — body `{ "action": "play"|"pause"|"next"|"previous" }`.
async fn music_control_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let (method, path) = match body["action"].as_str().unwrap_or("") {
        "play" => (reqwest::Method::PUT, "/me/player/play"),
        "pause" => (reqwest::Method::PUT, "/me/player/pause"),
        "next" => (reqwest::Method::POST, "/me/player/next"),
        "previous" => (reqwest::Method::POST, "/me/player/previous"),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Unknown action"})),
            )
                .into_response()
        }
    };

    let Some(resp) = spotify_api_call(&state, method, path).await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Spotify not connected"})),
        )
            .into_response();
    };

    match resp.status() {
        s if s.is_success() => Json(json!({"ok": true})).into_response(),
        StatusCode::NOT_FOUND => (
            StatusCode::CONFLICT,
            Json(json!({"error": "No active Spotify device. Open Spotify on a device first."})),
        )
            .into_response(),
        _ => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "Spotify request failed"})),
        )
            .into_response(),
    }
}

// ── Prompt Templates ─────────────────────────────────────────────────────────

async fn list_prompt_templates(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_template_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Prompt template repository not configured"})),
            )
                .into_response()
        }
    };
    match repo.list().await {
        Ok(templates) => Json(json!(templates)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn get_prompt_template(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_template_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Prompt template repository not configured"})),
            )
                .into_response()
        }
    };
    match repo.get(&name).await {
        Ok(Some(t)) => Json(json!(t)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Template not found"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct UpsertTemplateRequest {
    content: String,
    #[serde(default)]
    description: String,
}

async fn upsert_prompt_template(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Result<Json<UpsertTemplateRequest>, JsonRejection>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_template_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Prompt template repository not configured"})),
            )
                .into_response()
        }
    };
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let template = PromptTemplate {
        name: name.clone(),
        content: req.content,
        description: req.description,
        is_system: false,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    match repo.upsert(&template).await {
        Ok(()) => Json(json!({"name": name, "status": "ok"})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn delete_prompt_template(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_template_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Prompt template repository not configured"})),
            )
                .into_response()
        }
    };
    // Don't allow deletion of system templates
    if let Ok(Some(t)) = repo.get(&name).await {
        if t.is_system {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "Cannot delete built-in system templates"})),
            )
                .into_response();
        }
    }
    match repo.delete(&name).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn reset_prompt_template(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_template_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Prompt template repository not configured"})),
            )
                .into_response()
        }
    };

    // Only system (built-in) templates can be reset
    let (content, description) = match builtin_template_content(&name) {
        Some(pair) => pair,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("'{}' is not a built-in template. Only balanced | concise | technical | warm can be reset.", name)})),
            )
                .into_response()
        }
    };

    let template = PromptTemplate {
        name: name.clone(),
        content: content.to_string(),
        description: description.to_string(),
        is_system: true,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    match repo.upsert(&template).await {
        Ok(()) => Json(json!(template)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── Prompt Extras ─────────────────────────────────────────────────────────────

async fn list_prompt_extras(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_extra_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Prompt extra repository not configured"})),
            )
                .into_response()
        }
    };
    match repo.list_all().await {
        Ok(extras) => Json(json!(extras)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct UpsertExtraRequest {
    key: String,
    instruction: String,
    #[serde(default = "bool_true")]
    active: bool,
    #[serde(default)]
    sort_order: i32,
}

fn bool_true() -> bool {
    true
}

async fn upsert_prompt_extra(
    State(state): State<Arc<AppState>>,
    body: Result<Json<UpsertExtraRequest>, JsonRejection>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_extra_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Prompt extra repository not configured"})),
            )
                .into_response()
        }
    };
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let extra = PromptExtra {
        key: req.key.clone(),
        instruction: req.instruction,
        active: req.active,
        sort_order: req.sort_order,
    };
    match repo.upsert(&extra).await {
        Ok(()) => Json(json!({"key": req.key, "status": "ok"})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn delete_prompt_extra(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_extra_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Prompt extra repository not configured"})),
            )
                .into_response()
        }
    };
    match repo.delete(&key).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── Memories ──────────────────────────────────────────────────────────────────

async fn list_memories(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    match state.memory_repo.search_recent(None, 50).await {
        Ok(memories) => Json(json!(memories)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct SaveMemoryRequest {
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "default_source")]
    source: String,
    #[serde(default)]
    segment: Option<pond_core::user_data::domain::memory::MemorySegment>,
    #[serde(default)]
    importance: Option<f32>,
    #[serde(default)]
    tier: Option<pond_core::user_data::domain::memory::MemoryTier>,
}
fn default_source() -> String {
    "api".to_string()
}

async fn save_memory(
    State(state): State<Arc<AppState>>,
    body: Result<Json<SaveMemoryRequest>, JsonRejection>,
) -> impl axum::response::IntoResponse {
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    // Derive defaults from segment if provided
    let decay_rate = req
        .tier
        .as_ref()
        .map(|t| t.default_decay_rate())
        .or_else(|| {
            req.segment
                .as_ref()
                .map(|s| s.default_tier().default_decay_rate())
        });
    let importance = req
        .importance
        .or_else(|| req.segment.as_ref().map(|s| s.default_importance()));
    let tier = req
        .tier
        .or_else(|| req.segment.as_ref().map(|s| s.default_tier()));

    let fragment = MemoryFragment {
        id: Uuid::new_v4().to_string(),
        profile_id: None,
        session_id: None,
        content: req.content,
        embedding: None,
        source: req.source,
        tags: req.tags,
        created_at: chrono::Utc::now(),
        segment: req.segment,
        importance,
        tier,
        decay_rate,
        access_count: 0,
        last_accessed_at: None,
        lifecycle: Some(pond_core::user_data::domain::memory::MemoryLifecycle::Active),
        superseded_by: None,
        corrects: None,
    };
    match state.memory_repo.add(fragment.clone()).await {
        Ok(()) => (StatusCode::CREATED, Json(json!(fragment))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn delete_memory(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl axum::response::IntoResponse {
    match state.memory_repo.delete(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── Memory Consolidation ─────────────────────────────────────────────────────

/// POST /api/v1/memory/consolidate — manually trigger three-stage adversarial
/// consolidation. Returns an SSE stream of `ConsolidationEvent` so the UI can
/// show live progress (proposer -> adversary -> judge).
async fn start_consolidation(State(state): State<Arc<AppState>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    use pond_core::user_data::ports::memory_consolidator::ConsolidationEvent;

    let runner = match &state.consolidation_runner {
        Some(r) => r.clone(),
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Memory consolidation is not enabled"})),
            )
                .into_response();
        }
    };

    let cancel = tokio_util::sync::CancellationToken::new();
    *state.consolidation_cancel.write().await = Some(cancel.clone());

    // Subscribe BEFORE spawning so we don't miss the first event
    let mut rx = state.consolidation_event_tx.subscribe();

    // Spawn the injected consolidation runner
    tokio::spawn(runner(cancel));

    // Return an SSE stream backed by the broadcast receiver
    let stream = async_stream::stream! {
        while let Ok(event) = rx.recv().await {
            let json = serde_json::to_string(&event).unwrap_or_default();
            yield Ok::<_, Infallible>(Event::default().data(json));
            // Terminal events — stop streaming after these
            match event {
                ConsolidationEvent::Completed { .. }
                | ConsolidationEvent::Error { .. }
                | ConsolidationEvent::Cancelled => break,
                _ => {}
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// POST /api/v1/memory/consolidate/stop — cancel an in-progress consolidation.
async fn stop_consolidation(State(state): State<Arc<AppState>>) -> StatusCode {
    if let Some(cancel) = state.consolidation_cancel.read().await.as_ref() {
        cancel.cancel();
    }
    StatusCode::OK
}

// ── Skills ────────────────────────────────────────────────────────────────────

async fn list_skills(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    let repo = match &state.skill_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Skill repository not configured"})),
            )
                .into_response()
        }
    };
    match repo.list_all().await {
        Ok(skills) => Json(json!(skills)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct CreateSkillRequest {
    name: String,
    content: String,
}

async fn create_skill(
    State(state): State<Arc<AppState>>,
    body: Result<Json<CreateSkillRequest>, JsonRejection>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.skill_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Skill repository not configured"})),
            )
                .into_response()
        }
    };
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let skill = UserSkill {
        id: Uuid::new_v4().to_string(),
        name: req.name,
        content: req.content,
        active: true,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    match repo.create(&skill).await {
        Ok(()) => (StatusCode::CREATED, Json(json!(skill))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct UpdateSkillRequest {
    content: Option<String>,
    active: Option<bool>,
}

async fn update_skill(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<UpdateSkillRequest>, JsonRejection>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.skill_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Skill repository not configured"})),
            )
                .into_response()
        }
    };
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let existing = match repo.get(&id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Skill not found"})),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let updated = UserSkill {
        id: existing.id,
        name: existing.name,
        content: req.content.unwrap_or(existing.content),
        active: req.active.unwrap_or(existing.active),
        created_at: existing.created_at,
    };
    match repo.update(&updated).await {
        Ok(()) => Json(json!(updated)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn delete_skill(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.skill_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Skill repository not configured"})),
            )
                .into_response()
        }
    };
    match repo.delete(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── Recipes ───────────────────────────────────────────────────────────────────

async fn list_recipes(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    let repo = match &state.recipe_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Recipe repository not configured"})),
            )
                .into_response()
        }
    };
    match repo.list().await {
        Ok(recipes) => Json(json!(recipes)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct CreateRecipeRequest {
    name: String,
    #[serde(default)]
    description: String,
    yaml: String,
}

async fn create_recipe(
    State(state): State<Arc<AppState>>,
    body: Result<Json<CreateRecipeRequest>, JsonRejection>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.recipe_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Recipe repository not configured"})),
            )
                .into_response()
        }
    };
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let recipe = AgentRecipe {
        id: Uuid::new_v4().to_string(),
        name: req.name,
        description: req.description,
        yaml: req.yaml,
        active: true,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    match repo.upsert(&recipe).await {
        Ok(()) => (StatusCode::CREATED, Json(json!(recipe))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct UpdateRecipeRequest {
    description: Option<String>,
    yaml: Option<String>,
    active: Option<bool>,
}

async fn update_recipe(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<UpdateRecipeRequest>, JsonRejection>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.recipe_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Recipe repository not configured"})),
            )
                .into_response()
        }
    };
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let existing = match repo.get_by_id(&id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Recipe not found"})),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let updated = AgentRecipe {
        id: existing.id,
        name: existing.name,
        description: req.description.unwrap_or(existing.description),
        yaml: req.yaml.unwrap_or(existing.yaml),
        active: req.active.unwrap_or(existing.active),
        created_at: existing.created_at,
    };
    match repo.upsert(&updated).await {
        Ok(()) => Json(json!(updated)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn delete_recipe(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.recipe_repo {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Recipe repository not configured"})),
            )
                .into_response()
        }
    };
    match repo.delete(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize, Default)]
struct RunRecipeRequest {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    voice_mode: bool,
    #[serde(default)]
    canvas_mode: bool,
}

/// Execute a recipe by name. Looks up the AgentRecipe, parses its YAML to
/// extract the prompt, and delegates to the shared chat-stream pipeline so
/// the response matches `POST /api/v1/chat/stream` event-for-event.
async fn run_recipe(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Option<Json<RunRecipeRequest>>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)>
{
    let repo = state.recipe_repo.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"error": "Recipe repository not configured"})),
        )
    })?;

    let recipe = match repo.get_by_name(&name).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("Recipe '{}' not found", name)})),
            ))
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            ))
        }
    };

    if !recipe.active {
        tracing::warn!(name = %name, "running inactive recipe");
    }

    let prompt = match goose::recipe::Recipe::from_content(&recipe.yaml) {
        Ok(parsed) => parsed
            .prompt
            .or(parsed.instructions)
            .unwrap_or_else(|| format!("Run routine: {}", name)),
        Err(e) => {
            tracing::warn!(name = %name, error = %e, "failed to parse recipe YAML; using fallback prompt");
            format!("Run routine: {}", name)
        }
    };

    // Update activity timestamp — resets the consolidation inactivity timer
    *state.last_user_activity.write().await = std::time::Instant::now();
    if let Some(cancel) = state.consolidation_cancel.read().await.as_ref() {
        cancel.cancel();
    }

    let permit = state
        .sse_semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "Too many concurrent streams"})),
            )
        })?;

    let body = body.map(|Json(b)| b).unwrap_or_default();

    let chat_req = ChatRequest {
        session_id: body.session_id,
        message: prompt,
        images: Vec::new(),
        voice_mode: body.voice_mode,
        canvas_mode: body.canvas_mode,
    };

    Ok(chat_stream_inner(state, permit, chat_req))
}

// ───────────────────────── Face Biometrics (Phase 2) ────────────────────────
//
// Endpoints operate over multipart/form-data so the frontend can POST raw
// camera frames without a base64 round trip.
//
// Expected fields:
//   - `profile_id` (register only): text field naming the household member.
//   - `image`:      binary JPEG/PNG/WebP bytes.
//   - `bbox`:       optional `"x,y,w,h"` string naming the face crop region
//                   in source-pixel coordinates.  When absent the adapter
//                   falls back to a center-square crop (works for headshot
//                   framings; a real face detector should be wired in front
//                   for wide photos).
//
// When `AppState.face_recognition` is `None` (no ONNX model configured),
// every endpoint returns 503 Service Unavailable — callers should hide
// the biometric UI in that state.

fn face_unavailable() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": "Face recognition is not configured on this server",
            "hint":  "Install an ONNX face embedding model (see docs) and restart pond-server",
        })),
    )
}

/// Read `profile_id` + `image` + optional `bbox` out of a multipart body.
async fn read_face_multipart(
    mut multipart: Multipart,
) -> Result<
    (
        Option<String>,
        Vec<u8>,
        Option<pond_core::user_data::domain::face_recognition::BoundingBox>,
    ),
    (StatusCode, Json<Value>),
> {
    let mut profile_id: Option<String> = None;
    let mut image_bytes: Option<Vec<u8>> = None;
    let mut bbox: Option<pond_core::user_data::domain::face_recognition::BoundingBox> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("multipart error: {}", e)})),
        )
    })? {
        match field.name() {
            Some("profile_id") => {
                let text = field.text().await.map_err(|e| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": format!("read error: {}", e)})),
                    )
                })?;
                profile_id = Some(text);
            }
            Some("image") => {
                let bytes = field.bytes().await.map_err(|e| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": format!("read error: {}", e)})),
                    )
                })?;
                image_bytes = Some(bytes.to_vec());
            }
            Some("bbox") => {
                let text = field.text().await.map_err(|e| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": format!("read error: {}", e)})),
                    )
                })?;
                bbox =
                    pond_core::user_data::domain::face_recognition::BoundingBox::parse_csv(&text);
                if bbox.is_none() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "error": "invalid 'bbox' field — expected \"x,y,w,h\" unsigned integers"
                        })),
                    ));
                }
            }
            _ => {}
        }
    }

    let image = image_bytes.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing 'image' field in multipart body"})),
        )
    })?;
    Ok((profile_id, image, bbox))
}

/// POST /api/v1/faces/register — enroll a face for a household member.
///
/// Accepts `multipart/form-data` with `profile_id` and `image` fields.
/// Multiple enrollments per profile are allowed and recommended (3+ samples
/// per the acceptance criteria).
async fn register_face_handler(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(face_unavailable)?;

    let (profile_id, image, bbox) = read_face_multipart(multipart).await?;
    let profile_id = profile_id.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing 'profile_id' field"})),
        )
    })?;

    // Validate that the profile exists before touching biometric storage.
    match state.profile_repo.get(&profile_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("profile {} not found", profile_id)})),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("profile lookup failed: {}", e)})),
            ));
        }
    }

    let stored = face
        .register_face(&profile_id, &image, bbox)
        .await
        .map_err(|e| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error": e.to_string()})),
            )
        })?;

    Ok(Json(json!({
        "id":         stored.id,
        "profile_id": stored.profile_id,
        "model_dims": stored.model_dims,
        "created_at": stored.created_at,
    })))
}

/// POST /api/v1/faces/identify — identify a face against all enrolled profiles.
///
/// Accepts `multipart/form-data` with an `image` field.  Returns the best
/// matching profile when cosine similarity exceeds the configured threshold.
async fn identify_face_handler(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(face_unavailable)?;
    let (_profile_id, image, bbox) = read_face_multipart(multipart).await?;

    let result = face.identify_face(&image, bbox).await.map_err(|e| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": e.to_string()})),
        )
    })?;

    Ok(Json(json!({
        "identified": result.identified,
        "profile_id": result.profile_id,
        "confidence": result.confidence,
        "threshold":  face.match_threshold(),
    })))
}

/// GET /api/v1/faces/profile/:profile_id — list enrollments for a profile.
///
/// Returns embedding metadata (id, dims, timestamp) without the raw vector,
/// matching the privacy requirement that embeddings remain opaque BLOBs.
async fn list_face_enrollments(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(face_unavailable)?;
    let rows = face.list_embeddings(&profile_id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;

    let items: Vec<Value> = rows
        .iter()
        .map(|e| {
            json!({
                "id":         e.id,
                "profile_id": e.profile_id,
                "model_dims": e.model_dims,
                "created_at": e.created_at,
            })
        })
        .collect();

    Ok(Json(json!({
        "profile_id":  profile_id,
        "enrollments": items,
        "count":       rows.len(),
    })))
}

/// GET /api/v1/faces/models — report status of the three face models on disk.
///
/// Returns availability + size for each of the face-recognition model files.
///
/// Reports both the **preferred** model in each slot (AdaFace IR-101,
/// SCRFD 34G, Silent-Face V2, DeepPixBis) and the **fallback** files
/// from the buffalo_l bundle (ArcFace R50, SCRFD 10G), so the Models UI
/// can show "ready / fallback / missing" per slot.  Returns
/// `feature_enabled: false` when pond-server was built without the
/// `face-onnx` feature.
async fn list_face_models_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    let feature_enabled = state.face_recognition.is_some();
    let dir = state
        .data_dir
        .as_ref()
        .map(|d| d.join("models").join("face"));

    let dir_clone = dir.clone();
    let entries = tokio::task::spawn_blocking(move || {
        let describe = |dir: &Option<std::path::PathBuf>,
                        name: &str,
                        label: &str,
                        expected_mb: u64,
                        role: &str|
         -> Value {
            let path = dir.as_ref().map(|d| d.join(name));
            let (downloaded, size_mb) = match &path {
                Some(p) => match std::fs::metadata(p) {
                    Ok(md) => (true, Some(md.len() / 1_048_576)),
                    Err(_) => (false, None),
                },
                None => (false, None),
            };
            json!({
                "name": name,
                "label": label,
                "role": role,
                "expected_mb": expected_mb,
                "size_mb": size_mb,
                "downloaded": downloaded,
                "path": path.as_ref().map(|p| p.display().to_string()),
            })
        };

        vec![
            // Embedder slot — preferred + fallback.
            describe(
                &dir_clone,
                "adaface_ir101.onnx",
                "AdaFace IR-101 (preferred)",
                250,
                "embedding",
            ),
            describe(
                &dir_clone,
                "w600k_r50.onnx",
                "ArcFace R50 (fallback)",
                174,
                "embedding",
            ),
            // Detector slot — preferred + fallback.
            describe(
                &dir_clone,
                "scrfd_34g.onnx",
                "SCRFD 34G (preferred)",
                140,
                "detector",
            ),
            describe(
                &dir_clone,
                "scrfd.onnx",
                "SCRFD 10G (fallback)",
                17,
                "detector",
            ),
            // Anti-spoof ensemble.
            describe(
                &dir_clone,
                "antispoof.onnx",
                "Silent-Face V2 (primary PAD)",
                2,
                "antispoof",
            ),
            describe(
                &dir_clone,
                "OULU_Protocol_2_model_0_0.onnx",
                "DeepPixBis OULU-NPU (secondary PAD)",
                13,
                "antispoof",
            ),
        ]
    })
    .await
    .unwrap_or_default();

    Json(json!({
        "feature_enabled": feature_enabled,
        "models_dir": dir.as_ref().map(|d| d.display().to_string()),
        "models": entries,
    }))
}

/// GET /api/v1/faces/debug/pairwise — diagnostic: cross-sample cosine matrix.
///
/// Surfaces the full pairwise-cosine list plus a verdict:
///   * `healthy`             — within-profile pairs average high, cross low
///   * `collapsed`           — every pair (inc. cross-profile) > 0.90
///   * `cross_profile_leakage` — any cross-profile pair > 0.70
///   * `no_data`             — fewer than two stored embeddings
///
/// This is the single most useful lens when debugging "everyone matches at
/// ~0.6" regressions: it immediately tells you whether the model is
/// producing diverse embeddings or has collapsed under a preprocessing bug.
async fn face_pairwise_debug(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(face_unavailable)?;
    let pairs = face.pairwise_similarities().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;

    // Compute same-profile vs cross-profile summary stats.
    let same: Vec<f32> = pairs
        .iter()
        .filter(|p| p.same_profile)
        .map(|p| p.similarity)
        .collect();
    let cross: Vec<f32> = pairs
        .iter()
        .filter(|p| !p.same_profile)
        .map(|p| p.similarity)
        .collect();
    let mean = |v: &[f32]| -> Option<f32> {
        if v.is_empty() {
            None
        } else {
            Some(v.iter().sum::<f32>() / v.len() as f32)
        }
    };
    let max = |v: &[f32]| -> Option<f32> {
        v.iter()
            .copied()
            .fold(None, |acc, x| Some(acc.map_or(x, |a: f32| a.max(x))))
    };
    let min = |v: &[f32]| -> Option<f32> {
        v.iter()
            .copied()
            .fold(None, |acc, x| Some(acc.map_or(x, |a: f32| a.min(x))))
    };

    let verdict = if pairs.len() < 1 {
        "no_data"
    } else if pairs.iter().all(|p| p.similarity > 0.90) {
        "collapsed"
    } else if cross.iter().any(|&s| s > 0.70) {
        "cross_profile_leakage"
    } else {
        "healthy"
    };

    let items: Vec<Value> = pairs
        .iter()
        .map(|p| {
            json!({
                "id_a":        p.id_a,
                "id_b":        p.id_b,
                "profile_a":   p.profile_a,
                "profile_b":   p.profile_b,
                "similarity":  p.similarity,
                "same_profile": p.same_profile,
            })
        })
        .collect();

    Ok(Json(json!({
        "verdict":  verdict,
        "summary": {
            "same_profile":  { "count": same.len(),  "mean": mean(&same),  "min": min(&same),  "max": max(&same)  },
            "cross_profile": { "count": cross.len(), "mean": mean(&cross), "min": min(&cross), "max": max(&cross) },
        },
        "pairs": items,
        "threshold": face.match_threshold(),
    })))
}

/// GET /api/v1/faces/debug/eval — ROC-style calibration harness.
///
/// Uses the currently-stored pairwise similarities to:
///   * Compute FAR (false-accept rate) and FRR (false-reject rate) at a
///     sweep of candidate thresholds between 0.30 and 0.95.
///   * Report the EER-proxy (minimum of FAR+FRR), the threshold where the
///     two rates cross, and how they compare to the operator-configured
///     threshold returned by `face.match_threshold()`.
///
/// This is the single best-informed way to pick a threshold: "0.60 because
/// the spec said so" is a guess; "0.54, where cross-profile pairs drop
/// below 1% and same-profile pairs stay above 95%" is calibrated.  Works
/// only once there are enough enrollments to make the curve meaningful
/// (we require at least one same-profile pair and one cross-profile pair).
async fn face_eval_debug(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(face_unavailable)?;
    let pairs = face.pairwise_similarities().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;

    let same: Vec<f32> = pairs
        .iter()
        .filter(|p| p.same_profile)
        .map(|p| p.similarity)
        .collect();
    let cross: Vec<f32> = pairs
        .iter()
        .filter(|p| !p.same_profile)
        .map(|p| p.similarity)
        .collect();

    if same.is_empty() || cross.is_empty() {
        return Ok(Json(json!({
            "status": "insufficient_data",
            "hint":   "Need at least one same-profile and one cross-profile pair. Enroll two distinct members with ≥2 samples each.",
            "counts": { "same_profile": same.len(), "cross_profile": cross.len() },
            "threshold_in_use": face.match_threshold(),
        })));
    }

    // Sweep thresholds.  At each threshold:
    //   FAR = fraction of cross pairs with sim ≥ t  (should be LOW)
    //   FRR = fraction of same  pairs with sim <  t  (should be LOW)
    let n_same = same.len() as f32;
    let n_cross = cross.len() as f32;
    let mut curve: Vec<(f32, f32, f32)> = Vec::new(); // (t, FAR, FRR)
    let mut best_sum = f32::MAX;
    let mut best_threshold = face.match_threshold();
    let mut crossover: Option<(f32, f32)> = None; // (threshold, rate at crossover)

    for step in 0..=130 {
        let t = 0.30 + (step as f32) * 0.005; // 0.30 .. 0.95 in 0.005 steps
        let far = cross.iter().filter(|&&s| s >= t).count() as f32 / n_cross;
        let frr = same.iter().filter(|&&s| s < t).count() as f32 / n_same;
        let sum = far + frr;
        if sum < best_sum {
            best_sum = sum;
            best_threshold = t;
        }
        // Track the first crossover (FAR == FRR, approx) for the eer-ish
        // point used as a visual reference on the UI.
        if crossover.is_none() {
            if let Some(prev) = curve.last() {
                // Sign flip in (FAR - FRR) between consecutive samples.
                let prev_diff = prev.1 - prev.2;
                let cur_diff = far - frr;
                if prev_diff.signum() != cur_diff.signum()
                    && prev_diff.is_finite()
                    && cur_diff.is_finite()
                {
                    crossover = Some((t, (far + frr) / 2.0));
                }
            }
        }
        curve.push((t, far, frr));
    }

    let summary_mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
    let summary_max = |v: &[f32]| v.iter().copied().fold(f32::MIN, f32::max);
    let summary_min = |v: &[f32]| v.iter().copied().fold(f32::MAX, f32::min);

    Ok(Json(json!({
        "status": "ok",
        "counts": { "same_profile": same.len(), "cross_profile": cross.len() },
        "same_profile_stats":  {
            "mean": summary_mean(&same),
            "min":  summary_min(&same),
            "max":  summary_max(&same),
        },
        "cross_profile_stats": {
            "mean": summary_mean(&cross),
            "min":  summary_min(&cross),
            "max":  summary_max(&cross),
        },
        "threshold_in_use":   face.match_threshold(),
        "recommended_threshold": best_threshold,
        "recommended_sum_far_frr": best_sum,
        "crossover": crossover.map(|(t, r)| json!({ "threshold": t, "rate": r })),
        "curve": curve.iter().map(|(t, far, frr)| json!({
            "threshold": t, "far": far, "frr": frr
        })).collect::<Vec<_>>(),
        "note": "far = false-accept rate; frr = false-reject rate. Pick a \
                 threshold where far is small (≤1 %) and frr is acceptable \
                 for your use case; recommended_threshold minimises far+frr.",
    })))
}

/// Read multipart for the burst-identify endpoint.
///
/// Accepts repeated `image` fields (any number ≥ 1) plus an optional `bbox`
/// shared across the whole burst.  Returns one `Vec<u8>` per frame in order.
async fn read_face_multipart_burst(
    mut multipart: Multipart,
) -> Result<
    (
        Vec<Vec<u8>>,
        Option<pond_core::user_data::domain::face_recognition::BoundingBox>,
    ),
    (StatusCode, Json<Value>),
> {
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut bbox: Option<pond_core::user_data::domain::face_recognition::BoundingBox> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("multipart error: {}", e)})),
        )
    })? {
        match field.name() {
            Some("image") => {
                let bytes = field.bytes().await.map_err(|e| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": format!("read error: {}", e)})),
                    )
                })?;
                if !bytes.is_empty() {
                    frames.push(bytes.to_vec());
                }
            }
            Some("bbox") => {
                let text = field.text().await.map_err(|e| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": format!("read error: {}", e)})),
                    )
                })?;
                bbox =
                    pond_core::user_data::domain::face_recognition::BoundingBox::parse_csv(&text);
            }
            _ => {}
        }
    }

    if frames.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "expected one or more 'image' fields in multipart body"})),
        ));
    }
    // Cap the burst size to avoid runaway CPU on a single request.  At
    // ~80 ms per ArcFace inference plus detection overhead, 12 frames is the
    // ceiling that keeps the worst-case turn under one second on Jetson.
    if frames.len() > 12 {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "error": "too many frames in burst",
                "max":   12,
                "got":   frames.len(),
            })),
        ));
    }
    Ok((frames, bbox))
}

/// Parse an f32 env var, falling back to `default` on missing / unparseable.
fn env_or(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

/// Extract optional MCP-UI hint from tool result content.
///
/// MCP servers can optionally prepend a structured JSON marker to their
/// plain-text results. The marker format is:
///
/// ```text
/// [[[mcp-ui:card_type:{"key":"value",...}]]]
/// Clean text for the LLM continues here
/// ```
///
/// The SSE layer calls this before forwarding tool results. The LLM only
/// sees the clean text; the frontend gets a structured `"ui"` field in the
/// SSE event for rich rendering.
///
/// Returns `(clean_content_for_llm, optional_ui_hint_json)`.
fn extract_ui_hint(content: &str) -> (String, Option<serde_json::Value>) {
    let trimmed = content.trim_start();
    if let Some(rest) = trimmed.strip_prefix("[[[mcp-ui:") {
        if let Some(end_idx) = rest.find("]]]") {
            let hint_payload = &rest[..end_idx];
            let clean_text = rest[end_idx + 3..].trim_start().to_string();
            // hint_payload = "weather:{...}" -- split on first ':'
            if let Some(colon_idx) = hint_payload.find(':') {
                let card_type = &hint_payload[..colon_idx];
                let json_str = &hint_payload[colon_idx + 1..];
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(json_str) {
                    return (
                        clean_text,
                        Some(serde_json::json!({
                            "card_type": card_type,
                            "data": data
                        })),
                    );
                }
            }
            // Marker found but malformed JSON -- still strip it, no UI hint
            return (clean_text, None);
        }
    }
    (content.to_string(), None)
}

/// POST /api/v1/faces/identify-burst — multi-frame consensus identify.
///
/// Accepts N (1..=12) `image` fields representing successive camera frames
/// of the same subject.  Each frame is run through the full identify
/// pipeline; the per-frame results are then aggregated into a consensus:
///
///   * The **winning** profile is the one identified in the most frames
///     (ties broken by the higher mean confidence).
///   * The verdict is `identified=true` only when at least
///     `ceil(N * agreement_ratio)` frames agree on that profile, where the
///     ratio defaults to 0.6 (i.e. 3-of-5, 4-of-7) but can be tightened.
///   * `mean_confidence` reports the average cosine of the agreeing frames.
///
/// This blocks the "single lucky frame matched a stranger" failure mode the
/// single-shot endpoint can exhibit when the camera autofocus is mid-hunt
/// or the user is mid-blink.  It also makes a printed-photo attack harder
/// because a print presents *identical* embeddings across frames — high
/// agreement, but every frame trips the anti-spoof gate inside the
/// embedding extractor and is rejected as `no_face`.
async fn burst_identify_face_handler(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(face_unavailable)?;
    let (frames, bbox) = read_face_multipart_burst(multipart).await?;
    let n = frames.len();

    // Per-frame results (kept for the response so a UI can surface per-frame
    // diagnostics — useful when the consensus fails to explain *why*).
    let mut per_frame: Vec<Value> = Vec::with_capacity(n);
    let mut votes: std::collections::HashMap<String, (u32, f32)> = std::collections::HashMap::new(); // profile_id → (count, sum_confidence)
    let mut no_face_count = 0_u32;

    // Liveness side-channels: we accumulate the per-frame embedding and
    // landmark set so we can gate the burst on inter-frame motion *before*
    // trusting the identity consensus.  A held-up photo / phone screen
    // produces 12 near-identical embeddings and zero landmark jitter; a
    // real face does not.
    let mut frame_embeddings: Vec<Vec<f32>> = Vec::with_capacity(n);
    let mut frame_landmarks: Vec<pond_core::user_data::domain::face_recognition::FaceLandmarks> =
        Vec::with_capacity(n);

    for (idx, bytes) in frames.iter().enumerate() {
        match face.identify_with_diagnostics(bytes, bbox).await {
            Ok(details) => {
                let r = &details.identification;
                per_frame.push(json!({
                    "frame":      idx,
                    "identified": r.identified,
                    "profile_id": r.profile_id,
                    "confidence": r.confidence,
                }));
                if r.identified {
                    if let (Some(pid), Some(c)) = (r.profile_id.clone(), r.confidence) {
                        let entry = votes.entry(pid).or_insert((0, 0.0));
                        entry.0 += 1;
                        entry.1 += c;
                    }
                } else if r.confidence.is_none() {
                    no_face_count += 1;
                }
                if let Some(emb) = details.embedding {
                    frame_embeddings.push(emb);
                }
                if let Some(lm) = details.landmarks {
                    frame_landmarks.push(lm);
                }
            }
            Err(e) => {
                per_frame.push(json!({
                    "frame": idx,
                    "error": e.to_string(),
                }));
            }
        }
    }

    // ── Liveness gates (skipped for single-frame bursts) ────────────────
    let liveness = if n >= 3 {
        Some(compute_liveness_report(&frame_embeddings, &frame_landmarks))
    } else {
        None
    };

    if let Some(ref rep) = liveness {
        // Visible at info! so operators can see the actual numbers every
        // call produces — critical for tuning thresholds against real
        // webcams.  Pair with the matcher log to understand why a given
        // burst was accepted/rejected.
        tracing::info!(
            mean_inter_cos = rep.mean_inter_cos,
            landmark_motion = rep.landmark_motion,
            differential_motion = rep.differential_motion,
            eye_ratio_spread = rep.eye_ratio_spread,
            hard_reject = rep.hard_reject,
            suspicious = rep.suspicious,
            frames = n,
            "burst liveness report"
        );
        if rep.hard_reject {
            return Ok(Json(json!({
                "identified":       false,
                "reason":           "liveness_failed",
                "liveness":         rep.to_json(),
                "frames_total":     n,
                "no_face_frames":   no_face_count,
                "per_frame":        per_frame,
            })));
        }
    }

    // Consensus: highest vote count, ties broken by mean confidence.
    let winner: Option<(String, u32, f32)> = votes
        .clone()
        .into_iter()
        .map(|(pid, (count, sum))| (pid, count, sum / count as f32))
        .max_by(|a, b| {
            a.1.cmp(&b.1)
                .then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
        });

    // ─── Production-grade verification gate ────────────────────────────
    //
    // The matcher alone is not sufficient when only one profile is
    // enrolled — the runner-up margin and open-set gap are no-ops in that
    // regime, so a stranger whose embedding happens to land above the
    // global threshold slips through.  Burst verification closes this by
    // demanding multiple, independently converging signals:
    //
    //   (a) **Unanimity** — every frame that contained a face must
    //       identify the *same* profile.  A stranger occasionally pokes
    //       above threshold on one frame; unanimity across 5 frames at
    //       400 ms spacing happens with probability ≈ p^5, and a real
    //       match has p ≈ 1.
    //   (b) **Per-frame confidence floor** — every winning frame must
    //       individually clear `threshold + SINGLE_FRAME_MARGIN`, not
    //       just the mean.  Blocks the "three great frames + two bad
    //       frames averaging to pass" attack vector.
    //   (c) **Mean confidence floor** — the mean of winning frames must
    //       clear `threshold + MEAN_MARGIN` (0.03).  Kills the "barely
    //       5 × threshold" edge case.
    //   (d) **No-face budget** — if more than 20 % of frames produced no
    //       face / no embedding, the capture was too noisy to trust
    //       regardless of what the winning frames said.
    //   (e) **Suspicious-burst lockout** — soft-suspicious liveness
    //       (motion right at the floor) elevates the margins further.
    //
    // All margins are tunable via environment variables so operators can
    // move the ROC curve without recompiling.
    let suspicious = liveness.as_ref().map(|r| r.suspicious).unwrap_or(false);
    let threshold = face.match_threshold();

    // Empirical burst margins tuned against the w600k_r50 embedder:
    //   * Live face (real webcam): per-frame confidence clusters at 0.85–0.92
    //   * Photo attack (printed or screen): per-frame confidence lands at
    //     0.72–0.78 because the embedding of the photo differs slightly
    //     from the embedding of the real face due to lighting / sharpness /
    //     colour gamut differences.
    // A ~15-point gap exists.  Putting the floor at threshold+0.10 (= 0.80
    // when threshold=0.70) splits the middle cleanly: live clears it,
    // photos don't.  If you swap in a different embedder with tighter
    // calibration, lower these via env vars.
    let single_frame_margin = env_or("POND_FACE_BURST_FRAME_MARGIN", 0.10_f32);
    let mean_margin = env_or("POND_FACE_BURST_MEAN_MARGIN", 0.12_f32);
    let suspicious_extra = env_or("POND_FACE_BURST_SUSPICIOUS_MARGIN", 0.05_f32);
    let no_face_budget = env_or("POND_FACE_BURST_NOFACE_BUDGET", 0.20_f32);

    let frame_floor =
        threshold + single_frame_margin + if suspicious { suspicious_extra } else { 0.0 };
    let mean_floor = threshold + mean_margin + if suspicious { suspicious_extra } else { 0.0 };

    // Required vote count.  Single-frame bursts degrade to single-shot.
    // For n ≥ 3 we require **all face-bearing frames** to agree — which
    // after the no-face budget check below is effectively (n - no_face).
    let face_bearing = n as u32 - no_face_count;
    let no_face_ratio = if n == 0 {
        1.0
    } else {
        no_face_count as f32 / n as f32
    };

    // Extract per-frame confidences for the winning profile so we can
    // enforce (b) the individual-frame floor.
    let winner_pid_opt = winner.as_ref().map(|(p, _, _)| p.clone());
    let winner_frame_confs: Vec<f32> = winner_pid_opt
        .as_ref()
        .map(|target| {
            per_frame
                .iter()
                .filter_map(|pf| {
                    let pid = pf.get("profile_id").and_then(|v| v.as_str())?;
                    let conf = pf.get("confidence").and_then(|v| v.as_f64())?;
                    let identified = pf
                        .get("identified")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if identified && pid == target {
                        Some(conf as f32)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let min_winner_conf = winner_frame_confs
        .iter()
        .cloned()
        .fold(f32::INFINITY, f32::min);

    let required: u32 = if n == 1 { 1 } else { face_bearing.max(2) };

    let (identified, profile_id, mean_conf, votes_for_winner) = match &winner {
        Some((pid, count, mean))
            if *count >= required
                && no_face_ratio <= no_face_budget
                && *mean >= mean_floor
                && min_winner_conf >= frame_floor
                && winner_frame_confs.len() as u32 == *count =>
        {
            (true, Some(pid.clone()), Some(*mean), *count)
        }
        Some((pid, count, mean)) => {
            // Surface the would-be winner so callers can show "almost
            // matched X (2 of 5 frames)" instead of a bare null.
            (false, Some(pid.clone()), Some(*mean), *count)
        }
        None => (false, None, None, 0),
    };

    // Emit why a burst failed, when it failed — invaluable for tuning.
    if !identified {
        tracing::info!(
            n,
            no_face_count,
            no_face_ratio,
            required,
            mean_conf = ?mean_conf,
            min_winner_conf = if min_winner_conf.is_finite() { min_winner_conf } else { 0.0 },
            frame_floor,
            mean_floor,
            suspicious,
            votes_for_winner,
            "burst identify rejected"
        );
    }

    Ok(Json(json!({
        "identified":         identified,
        "profile_id":         if identified { profile_id.clone() } else { None },
        "candidate_profile":  profile_id,
        "mean_confidence":    mean_conf,
        "votes":              votes_for_winner,
        "frames_total":       n,
        "frames_required":    required,
        "no_face_frames":     no_face_count,
        "threshold":          threshold,
        "suspicious":         suspicious,
        "liveness":           liveness.as_ref().map(|r| r.to_json()),
        "per_frame":          per_frame,
    })))
}

/// Summary of the multi-frame liveness analysis.
///
/// `hard_reject` fires when the burst is almost certainly a presentation
/// attack (flat photo / phone screen) — inter-frame embedding cosine so
/// high, or landmark motion so low, that no real face could produce them.
/// `suspicious` is a softer signal: the burst is plausible but lives
/// close enough to the spoof floor that we want to require tighter
/// consensus before trusting it.
struct LivenessReport {
    hard_reject: bool,
    suspicious: bool,
    mean_inter_cos: f32,
    landmark_motion: f32,
    eye_ratio_spread: f32,
    /// Mean **non-rigid** per-landmark displacement in pixels across
    /// consecutive frame pairs.  A moving photo produces pure rigid
    /// translation (all five landmarks shift by the same vector), so
    /// subtracting the mean displacement across the five points leaves
    /// near-zero residual.  A live face produces non-rigid motion
    /// (independent blinks, mouth twitches, eyebrow raises) so the
    /// residual is ≥ 0.4 px even when overall motion is small.  This is
    /// the key photo-attack signal that survives an attacker waving the
    /// photo around to defeat `landmark_motion`.
    differential_motion: f32,
}

impl LivenessReport {
    fn to_json(&self) -> Value {
        json!({
            "hard_reject":         self.hard_reject,
            "suspicious":          self.suspicious,
            "mean_inter_cos":      self.mean_inter_cos,
            "landmark_motion":     self.landmark_motion,
            "eye_ratio_spread":    self.eye_ratio_spread,
            "differential_motion": self.differential_motion,
        })
    }
}

/// Compute liveness metrics for a burst of per-frame diagnostics.
///
/// Three signals, all derived from the already-computed landmarks and
/// embeddings — no extra inference:
///
/// 1. **Inter-frame embedding cosine.** Adjacent live-face frames differ
///    by 0.005–0.03 in cosine; a printed photo or LCD screen produces
///    > 0.995 across the burst.
///
/// 2. **Landmark motion.** Per-landmark pixel std-dev across the burst.
///    A still photo produces < 0.5 px (sensor noise only); a real face
///    micro-moves by ≥ 1.5 px.
///
/// 3. **Eye-ratio spread.** Inter-eye distance divided by inter-eye-to-
///    nose distance, range across the burst.  A blink / micro-expression
///    changes this by ≥ 0.5 %; a still photo keeps it flat.
fn compute_liveness_report(
    embeddings: &[Vec<f32>],
    landmarks: &[pond_core::user_data::domain::face_recognition::FaceLandmarks],
) -> LivenessReport {
    // (1) Inter-frame cosine similarity (mean over adjacent pairs).
    let mean_inter_cos = if embeddings.len() >= 2 {
        let mut sum = 0.0_f32;
        let mut count = 0_u32;
        for w in embeddings.windows(2) {
            if w[0].len() == w[1].len() && !w[0].is_empty() {
                let dot: f32 = w[0].iter().zip(&w[1]).map(|(a, b)| a * b).sum();
                // Embeddings are L2-normalised, so dot *is* cosine.
                sum += dot;
                count += 1;
            }
        }
        if count == 0 {
            0.0
        } else {
            sum / count as f32
        }
    } else {
        0.0
    };

    // (2) Landmark motion: std-dev of each of the five points in pixels,
    // averaged across points.  Each landmark is (x, y); combine x/y via
    // Euclidean per-frame deviation from the mean position.
    let landmark_motion = if landmarks.len() >= 2 {
        let n = landmarks.len() as f32;
        let mut total = 0.0_f32;
        let mut pts = 0_u32;
        for i in 0..5 {
            let point_of =
                |lm: &pond_core::user_data::domain::face_recognition::FaceLandmarks| -> (f32, f32) {
                    match i {
                        0 => lm.left_eye,
                        1 => lm.right_eye,
                        2 => lm.nose,
                        3 => lm.left_mouth,
                        _ => lm.right_mouth,
                    }
                };
            let (mx, my) = landmarks.iter().fold((0.0_f32, 0.0_f32), |(sx, sy), lm| {
                let (x, y) = point_of(lm);
                (sx + x, sy + y)
            });
            let (mx, my) = (mx / n, my / n);
            let var = landmarks
                .iter()
                .map(|lm| {
                    let (x, y) = point_of(lm);
                    (x - mx).powi(2) + (y - my).powi(2)
                })
                .sum::<f32>()
                / n;
            total += var.sqrt();
            pts += 1;
        }
        if pts == 0 {
            0.0
        } else {
            total / pts as f32
        }
    } else {
        0.0
    };

    // (3) Eye ratio spread — (inter-eye distance) / (eye-midpoint-to-nose),
    // min-max across the burst.
    let eye_ratio_spread = if landmarks.len() >= 2 {
        let ratios: Vec<f32> = landmarks
            .iter()
            .filter_map(|lm| {
                let eye_dx = lm.right_eye.0 - lm.left_eye.0;
                let eye_dy = lm.right_eye.1 - lm.left_eye.1;
                let eye_dist = (eye_dx * eye_dx + eye_dy * eye_dy).sqrt();
                let mid_x = (lm.left_eye.0 + lm.right_eye.0) * 0.5;
                let mid_y = (lm.left_eye.1 + lm.right_eye.1) * 0.5;
                let nose_dx = lm.nose.0 - mid_x;
                let nose_dy = lm.nose.1 - mid_y;
                let nose_dist = (nose_dx * nose_dx + nose_dy * nose_dy).sqrt();
                if nose_dist > 1e-3 {
                    Some(eye_dist / nose_dist)
                } else {
                    None
                }
            })
            .collect();
        if ratios.len() >= 2 {
            let min = ratios.iter().cloned().fold(f32::INFINITY, f32::min);
            let max = ratios.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let mean = ratios.iter().sum::<f32>() / ratios.len() as f32;
            if mean > 0.0 {
                (max - min) / mean
            } else {
                0.0
            }
        } else {
            0.0
        }
    } else {
        0.0
    };

    // Calibration note: ArcFace embeddings of the same person across 5
    // frames at ~400 ms intervals are inherently near-identical (we've
    // observed live cosines of 0.9974–0.9992 in repeated webcam tests),
    // so **inter-frame cosine is NOT discriminative** between a live
    // face and a photo at this burst length.  We keep it only as a
    // diagnostic signal in the response payload.
    //
    // The only signals that genuinely separate live from photo at 5
    // frames are:
    //
    //   * **Landmark motion** in pixels — live faces produce 2–15 px of
    //     jitter from breathing + micro-head-motion; a steady photo
    //     produces < 0.5 px.
    //   * **Eye-ratio spread** — live faces show 0.03–0.20 of geometric
    //     variance from blinks and expression; a photo shows < 0.005.
    //
    // We only hard-reject when **both** are at spoof levels, and with
    // a conservative floor — the cost of a false reject (user has to
    // retry and gets suspicious of the system) is higher than the cost
    // of a false accept here because the matcher threshold (0.70) and
    // the per-frame ONNX anti-spoof still stand in the way of a full
    // spoof match.
    // (4) Differential landmark motion.  For each consecutive frame pair,
    // compute the (dx, dy) displacement of each of the 5 landmarks, then
    // subtract the mean displacement across the five points (the rigid
    // component) and take the magnitude of the residual.  A photo being
    // translated / rotated produces near-zero residual; a live face's
    // independent eye / mouth motion produces ≥ 0.4 px per frame pair.
    //
    // This is the **key** signal for "photo being waved around" attacks:
    // `landmark_motion` is high (the whole face moves) but the motion is
    // rigid, so `differential_motion` stays low.
    let differential_motion = if landmarks.len() >= 2 {
        let mut sum = 0.0_f32;
        let mut pairs = 0_u32;
        for w in landmarks.windows(2) {
            let pts_a = [
                w[0].left_eye,
                w[0].right_eye,
                w[0].nose,
                w[0].left_mouth,
                w[0].right_mouth,
            ];
            let pts_b = [
                w[1].left_eye,
                w[1].right_eye,
                w[1].nose,
                w[1].left_mouth,
                w[1].right_mouth,
            ];
            // Per-landmark displacement.
            let disps: [(f32, f32); 5] = [
                (pts_b[0].0 - pts_a[0].0, pts_b[0].1 - pts_a[0].1),
                (pts_b[1].0 - pts_a[1].0, pts_b[1].1 - pts_a[1].1),
                (pts_b[2].0 - pts_a[2].0, pts_b[2].1 - pts_a[2].1),
                (pts_b[3].0 - pts_a[3].0, pts_b[3].1 - pts_a[3].1),
                (pts_b[4].0 - pts_a[4].0, pts_b[4].1 - pts_a[4].1),
            ];
            // Rigid component = mean displacement across the 5 landmarks.
            let mean_dx = disps.iter().map(|d| d.0).sum::<f32>() / 5.0;
            let mean_dy = disps.iter().map(|d| d.1).sum::<f32>() / 5.0;
            // Mean magnitude of residual (non-rigid) displacement.
            let residual = disps
                .iter()
                .map(|d| {
                    let rx = d.0 - mean_dx;
                    let ry = d.1 - mean_dy;
                    (rx * rx + ry * ry).sqrt()
                })
                .sum::<f32>()
                / 5.0;
            sum += residual;
            pairs += 1;
        }
        if pairs == 0 {
            0.0
        } else {
            sum / pairs as f32
        }
    } else {
        0.0
    };

    // (5) Face-size variation across the burst.  A live face moves slightly
    // closer to / further from the camera as the user breathes and shifts
    // their head, producing inter-eye distance variation of ~1.5–6 % across
    // 5 frames.  A photo held on a phone screen at arm's length stays at a
    // near-constant size (< 0.8 % variation).  This is the new signal that
    // catches the case the user reported: their own photo on a phone, with
    // slight hand jitter passing the existing motion floor but staying
    // dimensionally rigid.
    let face_size_spread = if landmarks.len() >= 2 {
        let dists: Vec<f32> = landmarks
            .iter()
            .map(|lm| {
                let dx = lm.right_eye.0 - lm.left_eye.0;
                let dy = lm.right_eye.1 - lm.left_eye.1;
                (dx * dx + dy * dy).sqrt()
            })
            .collect();
        let mean = dists.iter().sum::<f32>() / dists.len() as f32;
        if mean > 1e-3 {
            let min = dists.iter().cloned().fold(f32::INFINITY, f32::min);
            let max = dists.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            (max - min) / mean
        } else {
            0.0
        }
    } else {
        0.0
    };

    // Gates.
    //
    // Tunable via env so field-tuning doesn't require a rebuild:
    //   POND_FACE_LIVENESS_DIFF_MOTION_MIN   (default 0.60 px — was 0.35,
    //                                         raised because phone-screen
    //                                         photos with hand jitter can
    //                                         leak ~0.3 px non-rigid noise)
    //   POND_FACE_LIVENESS_MOTION_MIN        (default 0.5  px)
    //   POND_FACE_LIVENESS_EYE_SPREAD_MIN    (default 0.003 — was 0.002)
    //   POND_FACE_LIVENESS_SIZE_SPREAD_MIN   (default 0.012 — new: 1.2 % min
    //                                         inter-eye distance variation)
    //   POND_FACE_LIVENESS_INTER_COS_MAX     (default 0.9994 — new: phone-
    //                                         screen replays have ≥ 0.9995
    //                                         cosine because the same pixels
    //                                         are re-imaged each frame)
    let diff_floor = std::env::var("POND_FACE_LIVENESS_DIFF_MOTION_MIN")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.60);
    let motion_floor = std::env::var("POND_FACE_LIVENESS_MOTION_MIN")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.5);
    let eye_floor = std::env::var("POND_FACE_LIVENESS_EYE_SPREAD_MIN")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.003);
    let size_floor = std::env::var("POND_FACE_LIVENESS_SIZE_SPREAD_MIN")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.012);
    let cos_ceiling = std::env::var("POND_FACE_LIVENESS_INTER_COS_MAX")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.9994);

    let barely_moving = landmark_motion < motion_floor && landmarks.len() >= 3;
    let flat_eye_ratio = eye_ratio_spread < eye_floor;
    let rigid_motion = differential_motion < diff_floor && landmarks.len() >= 3;
    let dimensionally_rigid = face_size_spread < size_floor && landmarks.len() >= 3;
    let frozen_embedding = mean_inter_cos > cos_ceiling && embeddings.len() >= 3;

    // Hard reject on **any two** photo-like signals.  Previously we required
    // (flat_eye_ratio) to be one of them, which let a phone-screen photo with
    // slight zoom artefacts pass when its eye ratio happened to vary > 0.002.
    // The new "any two of five" rule is strictly stricter: a live face
    // typically fails ONE gate (e.g. brief still moment between blinks); a
    // photo fails THREE or more (eyes flat, size flat, rigid motion, frozen
    // embedding all at once).
    let photo_like = [
        barely_moving,
        flat_eye_ratio,
        rigid_motion,
        dimensionally_rigid,
        frozen_embedding,
    ]
    .iter()
    .filter(|x| **x)
    .count();
    let hard_reject = photo_like >= 2;

    // Soft suspicious tightens consensus on a single failed axis.
    let suspicious = !hard_reject && photo_like >= 1;

    LivenessReport {
        hard_reject,
        suspicious,
        mean_inter_cos,
        landmark_motion,
        eye_ratio_spread,
        differential_motion,
    }
}

/// POST /api/v1/faces/enroll-quality — pre-flight quality check for enrollment.
///
/// Accepts the same multipart payload as `/faces/register` (minus
/// `profile_id`) and reports whether the supplied frame is *good enough* to
/// enroll, without persisting anything.  The enrollment wizard calls this
/// per captured frame so it can show "good lighting ✓ / hold still" hints.
///
/// A frame passes when:
///   * a face is detected (we ran the same detector + alignment as the real
///     embedding pipeline), AND
///   * the embedding extractor returns a non-`None` vector, meaning the
///     frame cleared the variance / brightness / blur / anti-spoof gates.
///
/// The endpoint returns `ok=true/false` plus the reason on failure so the
/// UI can guide the user instead of silently rejecting their attempt.
async fn enroll_quality_handler(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(face_unavailable)?;
    let (profile_id, image, bbox) = read_face_multipart(multipart).await?;

    // Uses `identify_with_diagnostics` so we can get the embedding back and
    // measure how well it aligns with the profile's existing enrollments.
    // Self-consistency is the strongest operator-facing signal that an
    // enrollment is noisy: if three supposed "same person" frames disagree
    // with each other, the profile is going to false-reject or false-accept
    // unpredictably.
    let details = match face.identify_with_diagnostics(&image, bbox).await {
        Ok(d) => d,
        Err(e) => {
            return Ok(Json(json!({
                "ok":     false,
                "reason": format!("pipeline error: {}", e),
            })));
        }
    };

    let result = details.identification;
    let has_face = result.confidence.is_some() || details.embedding.is_some();

    // Fetch existing embeddings for this profile to score self-consistency.
    // Any error here is non-fatal — we still want to return the basic
    // quality verdict.
    let existing: Vec<pond_core::user_data::domain::face_recognition::FaceEmbedding> =
        match profile_id.as_deref() {
            Some(pid) => face.list_embeddings(pid).await.unwrap_or_default(),
            None => Vec::new(),
        };

    let existing_count = existing.len();
    let self_consistency = if existing.len() >= 2 {
        // Mean pairwise cosine across existing embeddings.  ≥ 0.70 is the
        // "same person, different captures" regime for ArcFace-aligned
        // 112×112; below that the operator should delete and re-enroll.
        let vecs: Vec<&Vec<f32>> = existing.iter().map(|e| &e.embedding).collect();
        let mut sum = 0.0_f32;
        let mut count = 0_u32;
        for i in 0..vecs.len() {
            for j in (i + 1)..vecs.len() {
                if vecs[i].len() == vecs[j].len() && !vecs[i].is_empty() {
                    let dot: f32 = vecs[i].iter().zip(vecs[j]).map(|(a, b)| a * b).sum();
                    // Stored embeddings are already L2-normalised.
                    sum += dot;
                    count += 1;
                }
            }
        }
        if count == 0 {
            None
        } else {
            Some(sum / count as f32)
        }
    } else {
        None
    };

    // Alignment: this new frame's embedding vs. existing centroid.  Gives
    // the UI an immediate "this shot looks like the enrolled person" read.
    let alignment_with_existing = match (&details.embedding, existing.len()) {
        (Some(q), n) if n >= 1 => {
            let dims = q.len();
            if dims == 0 {
                None
            } else {
                let mut centroid = vec![0.0_f32; dims];
                let mut counted = 0_u32;
                for e in &existing {
                    if e.embedding.len() == dims {
                        for (c, v) in centroid.iter_mut().zip(&e.embedding) {
                            *c += *v;
                        }
                        counted += 1;
                    }
                }
                if counted == 0 {
                    None
                } else {
                    let inv = 1.0 / counted as f32;
                    for c in &mut centroid {
                        *c *= inv;
                    }
                    let norm: f32 = centroid.iter().map(|v| v * v).sum::<f32>().sqrt();
                    if norm < 1e-6 {
                        None
                    } else {
                        for c in &mut centroid {
                            *c /= norm;
                        }
                        let dot: f32 = q.iter().zip(&centroid).map(|(a, b)| a * b).sum();
                        Some(dot)
                    }
                }
            }
        }
        _ => None,
    };

    // Recommended minimum aligned with `POND_FACE_MIN_SAMPLES` default (3).
    // Surface it so the UI can show "3 of 3 enrolled" progress.
    let recommended_min = std::env::var("POND_FACE_MIN_SAMPLES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(3);

    let consistency_warning = match self_consistency {
        Some(c) if c < 0.70 => Some(format!(
            "existing enrollments disagree with each other (mean pairwise cosine {:.2} \
             < 0.70 floor) — recommend deleting and re-enrolling with better lighting / pose",
            c
        )),
        _ => None,
    };

    let (ok, reason) = if has_face {
        (true, None)
    } else {
        (
            false,
            Some(
                "no usable face: detector found nothing or the frame was too dark, \
                 too blurry, too uniform, or flagged as a presentation attack"
                    .to_string(),
            ),
        )
    };

    Ok(Json(json!({
        "ok":       ok,
        "reason":   reason,
        "matches_existing":         result.identified,
        "matched_profile":          if result.identified { result.profile_id } else { None },
        "confidence":               result.confidence,
        "existing_samples":         existing_count,
        "recommended_min_samples":  recommended_min,
        "self_consistency":         self_consistency,
        "alignment_with_existing":  alignment_with_existing,
        "consistency_warning":      consistency_warning,
    })))
}

/// GET /api/v1/faces/profile/:profile_id/threshold — read per-profile override.
///
/// Returns `{ "profile_id", "threshold": <f32|null>, "global_threshold": <f32> }`.
/// `threshold = null` means no override is configured and `global_threshold`
/// applies to that profile.  Useful for the operator UI to render the
/// "use default / custom" toggle.
async fn get_profile_threshold_handler(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(face_unavailable)?;
    let override_t = face.get_profile_threshold(&profile_id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    Ok(Json(json!({
        "profile_id":       profile_id,
        "threshold":        override_t,
        "global_threshold": face.match_threshold(),
    })))
}

/// PUT /api/v1/faces/profile/:profile_id/threshold — set the override.
///
/// Body: `{ "threshold": 0.62, "note": "tightened after sibling false-match" }`.
/// `threshold` is required and clamped to [0.0, 1.0]; `note` is optional.
async fn put_profile_threshold_handler(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(face_unavailable)?;
    let threshold = body
        .get("threshold")
        .and_then(|v| v.as_f64())
        .map(|f| f as f32);
    let threshold = match threshold {
        Some(t) if (0.0..=1.0).contains(&t) => t,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "missing or invalid 'threshold' (expected float in [0.0, 1.0])"
                })),
            ))
        }
    };
    let note = body.get("note").and_then(|v| v.as_str());

    face.set_profile_threshold(&profile_id, Some(threshold), note)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;
    Ok(Json(json!({
        "profile_id": profile_id,
        "threshold":  threshold,
        "note":       note,
    })))
}

/// DELETE /api/v1/faces/profile/:profile_id/threshold — clear the override.
///
/// After this call the global threshold applies again for that profile.
async fn delete_profile_threshold_handler(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(face_unavailable)?;
    face.set_profile_threshold(&profile_id, None, None)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;
    Ok(Json(json!({ "profile_id": profile_id, "threshold": null })))
}

/// DELETE /api/v1/users/:profile_id/biometrics — forget all biometric data
/// for a household member.  Part of the Phase 3 privacy contract; implemented
/// for face embeddings now, extended to voice prints when Phase 1 lands.
async fn delete_user_biometrics(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(face_unavailable)?;
    let deleted = face.delete_embeddings(&profile_id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;

    Ok(Json(json!({
        "profile_id":      profile_id,
        "face_embeddings_deleted": deleted,
        // `voice_prints_deleted` will be populated once Phase 1 lands.
    })))
}

/// POST /api/v1/sessions/:session_id/identify-user — wake-on-face hook.
///
/// Accepts the same multipart payload as `/faces/identify` (plus optional
/// `bbox` field).  On a confident match the identified `profile_id` is
/// bound to the given session via the in-memory registry, so subsequent
/// chat turns can personalise the system prompt to the recognised user.
///
/// Returns the same body as `/faces/identify`, plus the `session_id` that
/// was bound.  `identified=false` leaves the binding untouched.
async fn identify_session_user_handler(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(face_unavailable)?;
    let (_profile_id, image, bbox) = read_face_multipart(multipart).await?;

    let result = face.identify_face(&image, bbox).await.map_err(|e| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": e.to_string()})),
        )
    })?;

    if result.identified {
        if let Some(pid) = result.profile_id.clone() {
            let mut guard = state.session_user_bindings.write().await;
            guard.insert(session_id.clone(), pid);
        }
    }

    Ok(Json(json!({
        "session_id": session_id,
        "identified": result.identified,
        "profile_id": result.profile_id,
        "confidence": result.confidence,
        "threshold":  face.match_threshold(),
    })))
}

/// GET /api/v1/sessions/:session_id/user — read the bound profile.
async fn get_session_user_handler(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let guard = state.session_user_bindings.read().await;
    let profile_id = guard.get(&session_id).cloned();
    Ok(Json(json!({
        "session_id": session_id,
        "profile_id": profile_id,
    })))
}

/// DELETE /api/v1/sessions/:session_id/user — release the binding.
async fn clear_session_user_handler(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut guard = state.session_user_bindings.write().await;
    let removed = guard.remove(&session_id).is_some();
    Ok(Json(json!({
        "session_id": session_id,
        "cleared":    removed,
    })))
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Memory-fit guard (Phase 6) ───────────────────────────────

    #[test]
    fn model_spills_budget_fits_small_model() {
        // gemma-2-2b (~1600 MB) fits a 4096 MB budget (effective 3072 after headroom).
        assert_eq!(model_spills_budget(1600, 4096), Some(false));
    }

    #[test]
    fn model_spills_budget_spills_large_model() {
        // gemma3n:e2b real download (~5600 MB) spills a 4096 MB budget.
        assert_eq!(model_spills_budget(5600, 4096), Some(true));
    }

    #[test]
    fn model_spills_budget_borderline_at_effective_boundary() {
        // Effective budget = 4096 - 1024 headroom = 3072.
        assert_eq!(model_spills_budget(3072, 4096), Some(false)); // exactly fits
        assert_eq!(model_spills_budget(3073, 4096), Some(true)); // one MB over
    }

    #[test]
    fn model_spills_budget_unknown_when_no_budget() {
        // NoopScheduler / Mac dev reports zero budget -- no verdict.
        assert_eq!(model_spills_budget(5600, 0), None);
    }

    #[test]
    fn model_spills_budget_unknown_when_size_unknown() {
        assert_eq!(model_spills_budget(0, 4096), None);
    }

    #[test]
    fn model_spills_budget_headroom_matches_desktop() {
        // MEMORY_FIT_HEADROOM_MB must mirror the desktop DEFAULT_HEADROOM_MB (1024)
        // so the server warning and the UI badge agree.
        assert_eq!(MEMORY_FIT_HEADROOM_MB, 1024);
    }

    #[test]
    fn derived_session_label_short_message_passthrough() {
        assert_eq!(
            derived_session_label("What is the weather today?"),
            "What is the weather today?"
        );
    }

    #[test]
    fn derived_session_label_collapses_whitespace() {
        assert_eq!(
            derived_session_label("  hello\n\n  there   world "),
            "hello there world"
        );
    }

    #[test]
    fn derived_session_label_caps_at_40_chars() {
        let long = "The quick brown fox jumps over the lazy dog again and again";
        let label = derived_session_label(long);
        // 40 chars of content + a trailing ellipsis marker.
        assert!(label.ends_with('…'), "expected ellipsis, got: {label}");
        assert!(
            label.chars().count() <= 41,
            "expected <=41 chars, got {}: {label}",
            label.chars().count()
        );
    }

    #[test]
    fn derived_session_label_strips_wrapping_quotes() {
        assert_eq!(derived_session_label("\"hello world\""), "hello world");
    }

    #[test]
    fn extract_ui_hint_with_valid_weather_hint() {
        let input = "[[[mcp-ui:weather:{\"temp\":64}]]]\nIt's sunny";
        let (clean, hint) = extract_ui_hint(input);
        assert_eq!(clean, "It's sunny");
        let ui = hint.expect("should have UI hint");
        assert_eq!(ui["card_type"], "weather");
        assert_eq!(ui["data"]["temp"], 64);
    }

    #[test]
    fn extract_ui_hint_no_marker_passthrough() {
        let input = "Plain text no hint";
        let (clean, hint) = extract_ui_hint(input);
        assert_eq!(clean, "Plain text no hint");
        assert!(hint.is_none());
    }

    #[test]
    fn extract_ui_hint_malformed_json_strips_marker() {
        let input = "[[[mcp-ui:weather:{not valid json}]]]\nClean text";
        let (clean, hint) = extract_ui_hint(input);
        assert_eq!(clean, "Clean text");
        assert!(hint.is_none(), "malformed JSON should not produce a hint");
    }

    #[test]
    fn extract_ui_hint_empty_content() {
        let input = "";
        let (clean, hint) = extract_ui_hint(input);
        assert_eq!(clean, "");
        assert!(hint.is_none());
    }

    #[test]
    fn extract_ui_hint_nested_json_data() {
        let input = "[[[mcp-ui:schedule:{\"schedules\":[{\"id\":\"abc\",\"name\":\"Morning\"}]}]]]\n- Morning [abc]: 0 0 8 * * *";
        let (clean, hint) = extract_ui_hint(input);
        assert_eq!(clean, "- Morning [abc]: 0 0 8 * * *");
        let ui = hint.expect("should have UI hint");
        assert_eq!(ui["card_type"], "schedule");
        assert_eq!(ui["data"]["schedules"][0]["id"], "abc");
    }

    #[test]
    fn extract_ui_hint_with_leading_whitespace() {
        let input = "  \n  [[[mcp-ui:weather:{\"temp\":72}]]]\nSunny day";
        let (clean, hint) = extract_ui_hint(input);
        assert_eq!(clean, "Sunny day");
        let ui = hint.expect("should parse hint despite leading whitespace");
        assert_eq!(ui["data"]["temp"], 72);
    }

    #[test]
    fn extract_ui_hint_marker_without_colon_in_payload() {
        // Marker present but payload has no colon separating type from JSON
        let input = "[[[mcp-ui:weather]]]\nSome text";
        let (clean, hint) = extract_ui_hint(input);
        assert_eq!(clean, "Some text");
        assert!(hint.is_none(), "missing colon should not produce a hint");
    }
}
