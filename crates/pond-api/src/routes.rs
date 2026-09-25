//! Route definitions for GIAP REST API and web dashboard.
//!
//! TODO: add request/response types in pond-core domain.

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
use pond_core::mesh::domain::millisats::Millisats;
use pond_core::mesh::domain::peer_id::PeerId as MeshPeerId;
use pond_core::mesh::domain::trust_scope::TrustScope;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::models::services::context::context_governor::{
    ContextGovernor, ContextInputs, EngineWindow,
};
use pond_core::models::services::context::context_monitor::ContextHealth;
use pond_core::prompts::{builtin_template_content, ProfileContext};
use pond_core::security::domain::event::{EventCategory, EventQuery, PrivacySensitivity};
use pond_core::security::domain::proven_device::{DeviceRung, ProvenDevice};
use pond_core::security::ports::handshake::{
    ChallengeResponse, HandshakeRequest, HandshakeResponse, InitRequest, RefreshRequest,
    VerifyRequest,
};
use pond_core::shared::ports::event_bus::BusEvent;
use pond_core::shared::services::chat::ChatService;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::domain::schedule::{Schedule, SensorTriggerSpec, TaskKind};
use pond_core::user_data::domain::sensor::{CameraEvent, SensorReading};
use pond_core::user_data::domain::session::{IdentificationSource, SessionIdentity};
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_core::user_data::ports::scheduler::{CreateScheduleRequest, UpdateScheduleRequest};
use pond_core::user_data::ports::session_storage::SessionStorageError;
use pond_core::user_data::services::identity_resolution;
use pond_core::user_data::services::onboarding::OnboardingService;
use serde::{Deserialize, Serialize};
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
use pond_core::context::ports::ContextRepository;
use pond_core::security::ports::policy::is_draft_decision_permitted;
use pond_core::user_data::domain::draft::DraftStatus;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::proposal::{ProposalAudience, PROPOSAL_SESSION_ID};
use pond_core::user_data::ports::draft::DraftRepository;
use pond_core::user_data::ports::proposal::ProposalRepository;

// ───────────────────────── REST API Routes ─────────────────────────

/// Builds the full REST API router with onboarding-aware middleware
pub fn api_routes(state: Arc<AppState>) -> Router<Arc<AppState>> {
    // ───────────── Public routes (accessible before onboarding) ─────────────
    // "Public" = not behind `require_onboarding_complete`. Anonymous access is separate
    // (`middleware::PUBLIC_ROUTES`): the wizard's writes (`PUT /settings`,
    // `POST /profiles`, `PATCH /profiles/{id}`, `POST /onboard/*`,
    // `/voice/calibrate`) close to anonymous callers after onboarding. Classify new routes there.
    let public_routes = Router::new()
        .route("/health", get(health))
        .route("/handshake", post(handshake_handler))
        // Two-phase pairing: init → verify; `pairing-code` is loopback-only.
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
        // Per-step progress, so the wizard can resume mid-onboarding.
        .route("/onboard/step/{name}", post(onboarding_step))
        // Reset onboarding back to the first step ("Start over" in Settings).
        .route("/onboard/reset", post(reset_onboarding))
        // Settings write is public so onboarding steps can save before completion
        .route("/settings", put(update_settings))
        // Warm-up readiness; anonymous during onboarding, like the /settings write (PUBLIC_ROUTES).
        .route("/warmup", get(get_warmup))
        // ── Time and place ────────────────────────────────────────────────
        // Public: the wizard sets location before any device pairs (see PUBLIC_ROUTES).
        .route("/time/zones", get(list_time_zones))
        .route("/location/detect", post(detect_location))
        // Public for the onboarding voice preview; local text→audio leaks no user data.
        .route("/tts", post(tts_synthesise))
        // Transcription proxy (public — local test tool)
        .route("/transcribe", post(transcribe))
        .route("/voice/tts/apply", post(apply_tts_settings))
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
        .route("/chat/runs/{run_id}/events", get(reattach_run_events))
        .route("/chat/runs/{run_id}/cancel", post(cancel_run))
        // By session id: a restarted client knows nothing else.
        .route(
            "/sessions/{session_id}/active-run",
            get(session_active_run).delete(cancel_session_run),
        )
        // Axum's 2 MiB default body limit is below a legal attachment set and would reject it
        // before `image_limit_response` can explain; raised for this route only.
        .route(
            "/chat/stream",
            post(chat_stream).layer(axum::extract::DefaultBodyLimit::max(
                pond_core::models::domain::image_limits::MAX_CHAT_BODY_BYTES,
            )),
        )
        .route("/sessions", get(list_sessions))
        .route(
            "/sessions/{session_id}",
            patch(rename_session).delete(delete_session),
        )
        .route("/sessions/{session_id}/messages", get(get_session_messages))
        .route("/sessions/{session_id}/compact", post(compact_session))
        .route("/sessions/retitle", post(retitle_sessions))
        .route("/sessions/{session_id}/retitle", post(retitle_session))
        // Deletes this and every later message: the client's edit/refresh primitive.
        .route(
            "/sessions/{session_id}/messages/{message_id}",
            delete(delete_messages_from_handler),
        )
        // Like/dislike training-feedback on one message.
        .route(
            "/sessions/{session_id}/messages/{message_id}/feedback",
            put(set_message_feedback_handler),
        )
        .route(
            "/sessions/{session_id}/attachments/{attachment_id}",
            get(get_session_attachment),
        )
        .route("/usage/summary", get(usage_summary))
        .route("/devices", get(list_devices).post(register_device))
        .route("/devices/commission", post(commission_device))
        .route("/matter/status", get(matter_status))
        .route(
            "/devices/{id}",
            axum::routing::delete(unregister_device).put(update_device),
        )
        .route("/devices/{id}/heartbeat", post(device_heartbeat))
        .route("/devices/{id}/offline", post(device_offline))
        .route(
            "/devices/{id}/push-token",
            post(register_push_token).delete(delete_push_token),
        )
        // Private mesh: trust-circle peers + self invite.
        .route("/mesh/peers", get(list_mesh_peers).post(add_mesh_peer))
        .route(
            "/mesh/peers/{peer_id}",
            axum::routing::delete(remove_mesh_peer),
        )
        .route("/mesh/peers/{peer_id}/credit", post(credit_mesh_peer))
        .route(
            "/mesh/peers/{peer_id}/capabilities",
            get(get_mesh_peer_capabilities),
        )
        .route("/mesh/self", get(get_mesh_self))
        .route("/mesh/settlement", get(get_mesh_settlement_status))
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
        .route("/models/download/control", post(download_control))
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
        // The unified event log; DELETE clears it ("clear my activity").
        .route("/activity", get(get_activity).delete(clear_activity))
        .route("/activity/summary", get(activity_summary))
        // Would-deny policy telemetry; must stay out of PUBLIC_ROUTES.
        .route("/security/policy-report", get(policy_report))
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
        // ── Sensor rules ───────────────────────────────────────────────────
        // A rule is a `SensorTrigger` schedule. Unlike `/schedules`, these validate the spec
        // and refuse to reach a cron schedule by id.
        .route("/rules", get(list_rules).post(create_rule))
        .route(
            "/rules/{id}",
            get(get_rule).put(update_rule).delete(delete_rule),
        )
        .route("/rules/{id}/pause", post(pause_rule))
        .route("/rules/{id}/resume", post(resume_rule))
        // ── Proactive proposals ────────────────────────────────────────────
        // Keep out of `middleware::PUBLIC_ROUTES`: a proposal is addressed to one member.
        .route("/proposals", get(list_proposals))
        .route(
            "/context/sources",
            get(list_context_sources).post(connect_context_source),
        )
        .route("/context/sources/{id}", delete(disconnect_context_source))
        .route("/context/sync", post(sync_context_sources))
        .route("/context/items", get(list_context_items))
        // ── The index's own health, and the way to repair it ───────────────
        // Keep out of PUBLIC_ROUTES: the counts describe a household, and rebuild is costly.
        .route("/context/index/health", get(context_index_health))
        .route("/context/index/rebuild", post(rebuild_context_index))
        .route("/proposals/{id}/decide", post(decide_proposal))
        // Foreground push: per-device notification stream.
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
        .route("/oauth/status/{state}", get(oauth_status_handler))
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
        // Same body ceiling as /chat/stream — this route accepts `images` too.
        .route(
            "/agent/chat/stream",
            post(agent_chat_stream).layer(axum::extract::DefaultBodyLimit::max(
                pond_core::models::domain::image_limits::MAX_CHAT_BODY_BYTES,
            )),
        )
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
        .route("/memories/{id}", delete(delete_memory).put(update_memory))
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
        // ── Face biometrics ───────────────────────────────────────────────────
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
            get(get_session_user_handler)
                .put(set_session_user_handler)
                .delete(clear_session_user_handler),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_onboarding_complete,
        ));

    public_routes.merge(protected_routes).with_state(state)
}

// ───────────────────────── Web Dashboard Routes ─────────────────────

/// Web UI embedded at build time (`build.rs` + `vite build`); a placeholder until built.
static WEB_DIST: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../pond-desktop/dist");

/// True when a *real* built UI is embedded (not the build.rs placeholder).
pub(crate) fn embedded_ui_present() -> bool {
    WEB_DIST
        .get_file("index.html")
        .map(|f| {
            !f.contents()
                .windows(b"data-giap-placeholder".len())
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

/// Serves the embedded UI, else `static_dir` (dev); extensionless paths get `index.html`.
pub async fn serve_web(uri: axum::http::Uri, static_dir: std::path::PathBuf) -> impl IntoResponse {
    let raw = uri.path().trim_start_matches('/');
    let rel = if raw.is_empty() { "index.html" } else { raw };

    if embedded_ui_present() {
        if let Some(file) = WEB_DIST.get_file(rel) {
            return file_response(rel, file.contents().to_vec());
        }
        if !rel.contains('.') {
            if let Some(index) = WEB_DIST.get_file("index.html") {
                return file_response("index.html", index.contents().to_vec());
            }
        }
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }

    serve_from_disk(&static_dir, rel).await
}

async fn serve_from_disk(static_dir: &std::path::Path, rel: &str) -> Response<axum::body::Body> {
    // Only plain components: `Path::starts_with` is lexical, so `dir/..` would pass it.
    let contained = std::path::Path::new(rel).components().all(|c| {
        matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    });
    if contained {
        if let Ok(bytes) = tokio::fs::read(static_dir.join(rel)).await {
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

/// Handshake endpoint to get authentication token (public).
/// TODO: full GIAP ↔ GOTG handshake (verify client, exchange token, return connection details).
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

/// Logs the error and returns a generic 500, so internal error text never reaches callers.
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

/// Stricter per-IP limiter for the brute-forceable `/handshake/verify`; a client pairs once.
fn verify_limiter() -> &'static crate::middleware::RateLimiter {
    static VERIFY_LIMITER: std::sync::OnceLock<crate::middleware::RateLimiter> =
        std::sync::OnceLock::new();
    VERIFY_LIMITER
        .get_or_init(|| crate::middleware::RateLimiter::new(10, std::time::Duration::from_secs(60)))
}

/// Logs, records and notifies a pairing outcome; best-effort, never alters the response.
/// `reason` is a closed set of rejection codes, safe to log: none carries the code or MAC.
async fn emit_pairing_outcome(
    state: &AppState,
    paired: bool,
    device_name: Option<&str>,
    reason: Option<&str>,
) {
    use pond_core::security::domain::event::{Event, EventCategory, PrivacySensitivity};

    let action = if paired {
        "auth.device_paired"
    } else {
        "auth.pairing_verify_failed"
    };

    if paired {
        tracing::info!(
            device = device_name.unwrap_or("unnamed"),
            "pairing accepted"
        );
    } else {
        tracing::warn!(
            device = device_name.unwrap_or("unnamed"),
            reason = reason.unwrap_or("unspecified"),
            "pairing rejected"
        );
    }

    if let Some(event_log) = state.event_log.as_ref() {
        let mut event =
            Event::new(EventCategory::Auth, action).sensitivity(PrivacySensitivity::Sensitive);
        if let Some(name) = device_name {
            event = event.attr("device_name", name);
        }
        if let Some(reason) = reason.filter(|_| !paired) {
            event = event.attr("rejection_reason", reason);
        }
        if let Err(e) = event_log.append(event).await {
            tracing::warn!(error = %e, action, "failed to record pairing event");
        }
    }

    let Some(sender) = state.notification_sender.as_ref() else {
        return;
    };
    // Debounce alerts (not events): one phone alert per window, not one per brute-force guess.
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
    // Per source IP, loopback included.
    if let Err(remaining) = verify_limiter()
        .check_rate_limit_detailed(&peer.ip().to_string())
        .await
    {
        let retry_after = crate::middleware::retry_after_secs(remaining);
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": "too many handshake attempts; slow down",
                "retry_after_secs": retry_after,
            })),
        ));
    }
    let Json(request) = body.map_err(|_| bad_body())?;
    let device_name = request.device_name.clone();
    let resp = match state.handshake.verify_handshake(request).await {
        Ok(resp) => resp,
        Err(e) => {
            emit_pairing_outcome(
                &state,
                false,
                device_name.as_deref(),
                Some("internal_error"),
            )
            .await;
            return Err(handshake_error("verify", e));
        }
    };
    emit_pairing_outcome(
        &state,
        resp.accepted,
        device_name.as_deref(),
        resp.rejection_reason.as_deref(),
    )
    .await;
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

/// Re-displays the current pairing code. Loopback-only: the host's CLI/dashboard.
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
        Some(pc) => Ok(Json(json!({
            "code": pc.code,
            "expires_at": pc.expires_at,
            // Whose device this code will make; `null` = unattributed.
            "profile_id": pc.profile_id,
        }))),
        None => Ok(Json(json!({"code": null}))),
    }
}

/// Optional body of `POST /handshake/pairing-code`. The member is bound here, at loopback
/// issuance, never by the pairing client: `PairedDevice` outranks all other identification.
/// `deny_unknown_fields` so a misspelt field fails instead of issuing a code bound to nobody.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct IssuePairingCodeRequest {
    /// Owning member; when omitted, a one-member household defaults to its sole member.
    #[serde(default)]
    profile_id: Option<String>,
    /// Bind to nobody even when a member could be inferred (e.g. a guest's phone).
    #[serde(default)]
    unattributed: bool,
}

/// Issues a fresh single-use pairing code, optionally bound to a member. Loopback-only.
async fn handshake_issue_pairing_code(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !peer.ip().is_loopback() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "pairing codes can only be issued on the host"})),
        ));
    }
    // Raw bytes, not `Json`: the CLI and dashboard POST with no body or content type. A present
    // but unreadable body is refused, not defaulted, so a mistyped binding can't silently issue.
    let request: IssuePairingCodeRequest = if body.is_empty() {
        IssuePairingCodeRequest::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!("invalid pairing-code request body: {e}"),
                })),
            )
        })?
    };

    let named_a_member = request.profile_id.is_some();
    // A failed read narrows to unattributed: a wrong owner would ride on every turn.
    let member_ids: Vec<String> = match state.profile_repo.list().await {
        Ok(profiles) => profiles.into_iter().map(|p| p.id).collect(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "could not list household members; issuing an unattributed pairing code"
            );
            Vec::new()
        }
    };
    let owner = pond_core::user_data::services::member_attribution::owner_for_new_code(
        request.profile_id.as_deref(),
        request.unattributed,
        &member_ids,
    );
    if owner.is_some() && !named_a_member {
        tracing::info!(
            "this household has one member, so the device pairing with this code becomes theirs; \
             POST {{\"unattributed\": true}} for a code that binds to nobody"
        );
    }
    let pc = state
        .handshake
        .issue_pairing_code_for(owner.as_deref())
        .await
        .map_err(|e| {
            // An unknown member id trips migration 0043's FK: a 400, not a 500.
            let internal = handshake_error("issue_pairing_code", e);
            if named_a_member {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": "could not issue a pairing code for that household member",
                    })),
                )
            } else {
                internal
            }
        })?;
    Ok(Json(json!({
        "code": pc.code,
        "expires_at": pc.expires_at,
        "profile_id": pc.profile_id,
    })))
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

/// Marks onboarding complete (public), lifting the onboarding guard on protected routes.
async fn complete_onboarding(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // A half-set-up assistant must not lift the guard: it would land in a broken dashboard.
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

/// Onboarding-status JSON shared by the status, step and reset routes so their shapes match.
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

/// Records the wizard reaching an onboarding step (public); progress only moves forward.
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

    // Completion must go through `/onboard/complete`, which validates required settings.
    if requested.is_complete() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Use POST /onboard/complete to finish onboarding",
            })),
        ));
    }

    let service = OnboardingService::new(state.onboarding_repo.clone());

    let target = service.advance_to(requested).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save onboarding step: {}", e)})),
        )
    })?;

    Ok(Json(onboarding_status_json(Some(target))))
}

/// Resets onboarding to `Welcome` and re-arms the onboarding guard (public).
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
    /// Canvas mode: prefer tool calls so results render as visual cards.
    #[serde(default)]
    canvas_mode: bool,
    /// Restricts this turn to these tool-group prefixes; `run_recipe` sets it from `extensions:`.
    #[serde(default)]
    tool_group_allowlist: Option<Vec<String>>,
    /// Keep running after disconnect, re-attachable via `/chat/runs/{run_id}/events`. Must
    /// default to false: voice cancels speculative turns by dropping the socket.
    #[serde(default)]
    resumable: bool,
}

/// Non-streaming chat turn; creates the session when `session_id` is absent.
async fn chat(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    body: Result<Json<ChatRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // From the principal, before the body is parsed, so no body field can stand in for it.
    let device = proven_device(principal.as_ref());

    // Resets the inactivity clock and interrupts any background consolidation.
    state.note_user_activity().await;

    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;

    let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let storage = &state.session_storage;

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

    // Scope resolved exactly as /chat/stream does, so both endpoints agree on the speaker.
    let service = ChatService::new(state.agent.clone(), session_id.clone(), storage.clone())
        .with_profile_scope(resolve_turn_scope(&state, &session_id, &device).await);

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

// ── Applying voice settings to the running engine ─────────────────────────────

#[derive(Deserialize)]
struct ApplyTtsRequest {
    voice: Option<String>,
    /// Pace multiplier. Clamped by the engine, not here.
    speed: Option<f32>,
    quality: Option<String>,
}

/// Applies voice settings to the live engine, downloading anything missing first.
/// Omitted fields fall back to the saved settings.
async fn apply_tts_settings(
    State(state): State<Arc<AppState>>,
    body: Option<Json<ApplyTtsRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(control) = state.tts_control.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "No speech engine is running"})),
        ));
    };

    let saved = state.settings_repo.get().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    let req = body.map(|Json(b)| b);
    let voice = req
        .as_ref()
        .and_then(|b| b.voice.clone())
        .unwrap_or(saved.voice_tts_voice);
    let speed = req
        .as_ref()
        .and_then(|b| b.speed)
        .unwrap_or(saved.voice_tts_speed);
    let quality = req
        .as_ref()
        .and_then(|b| b.quality.clone())
        .unwrap_or(saved.voice_tts_quality);

    let applied = control.apply(&voice, speed, &quality).await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
    })?;

    Ok(Json(json!({
        "voice": applied.voice,
        "speed": applied.speed_milli as f32 / 1000.0,
        "quality": applied.quality,
        "downloaded_voice": applied.downloaded_voice,
        "downloaded_weights": applied.downloaded_weights,
        "engine_reloaded": applied.engine_reloaded,
        "installed_voices": control.installed_voices().await,
    })))
}

// ── TTS request ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TtsRequest {
    text: String,
}

/// Synthesises WAV: in-process `AppState.tts` first, else the legacy Piper HTTP server.
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

    if let Some(tts) = &state.tts {
        match tts.synthesize(text).await {
            Ok(Some(wav_bytes)) => {
                return Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "audio/wav")
                    .body(Body::from(wav_bytes))
                    .map_err(|e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({"error": format!("Failed to build TTS response: {}", e)})),
                        )
                    });
            }
            Ok(None) => {
                // No audio: fall through to the legacy HTTP path.
            }
            Err(e) => {
                tracing::warn!("in-process TTS synthesis failed, trying legacy HTTP path: {e}");
            }
        }
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

// ── SSE streaming chat ────────────────────────────────────────────────────────

/// Streams a chat turn as SSE JSON frames, ending with `{"done": true, ...}` once persisted.
async fn chat_stream(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    body: Result<Json<ChatRequest>, JsonRejection>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)>
{
    // From the principal, never the body: `ChatRequest` must never grow a device field.
    let device = proven_device(principal.as_ref());

    // Resets the inactivity clock and interrupts any background consolidation.
    state.note_user_activity().await;

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

    // Before the stream opens, so the client gets a real HTTP status, not an SSE error event.
    image_limit_response(&req.images)?;

    if !req.resumable {
        // Ends with its last reader. Unregistered: it must not consume the detached-run cap.
        let run = new_run(&req, &device, crate::runs::RunPolicy::Ephemeral);
        return Ok(spawn_run(state, permit, run, None, req, device));
    }

    // Not `sse_semaphore` (which counts readers): a detached run outlives its reader, and
    // abandoned runs must not starve interactive chat. Taken before the stream, for a 503.
    let run_permit = state
        .runs
        .permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            tracing::warn!(
                target: "giap::runs",
                active = state.runs.registry.len(),
                "refused a resumable turn: the detached-run cap is full"
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "Too many agent runs in flight",
                    "kind": "run_cap",
                })),
            )
        })?;

    let run = new_run(&req, &device, crate::runs::RunPolicy::Detached);
    state.runs.registry.insert(run.clone()).map_err(|e| {
        let crate::runs::RegistryFull::AtCap { active, max } = e;
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": format!("Too many agent runs in flight ({active}/{max})"),
                "kind": "run_cap",
            })),
        )
    })?;

    // Tells a reconnecting client what to reattach to; a run id from another epoch is dead.
    run.push(
        json!({
            "type": "run_started",
            "run_id": run.run_id,
            "session_id": run.session_id,
            "epoch": state.runs.epoch,
            "resumable": true,
        })
        .to_string(),
        false,
    );

    Ok(spawn_run(state, permit, run, Some(run_permit), req, device))
}

/// Maps an image-limit violation onto an HTTP status, or passes a legal set through.
fn image_limit_response(
    images: &[pond_core::models::domain::message::ImageAttachment],
) -> Result<(), (StatusCode, Json<Value>)> {
    use pond_core::models::domain::image_limits::{validate_turn_images, ImageLimitError};

    match validate_turn_images(images) {
        Ok(()) => Ok(()),
        Err(e) => {
            let status = if e.is_too_large() {
                StatusCode::PAYLOAD_TOO_LARGE
            } else if matches!(e, ImageLimitError::UnsupportedMimeType { .. }) {
                StatusCode::UNSUPPORTED_MEDIA_TYPE
            } else {
                StatusCode::BAD_REQUEST
            };
            Err((status, Json(json!({"error": e.to_string()}))))
        }
    }
}

// ── One engine event, one SSE frame ───────────────────────────────────────────

/// What one `AgentStreamEvent` means to a chat SSE stream, for both stream handlers.
/// `Reasoning` and `TurnComplete` return as data because that's where the routes differ.
#[derive(Debug, PartialEq)]
enum StreamStep {
    /// Emit nothing; the thought filter held the whole chunk back.
    Nothing,
    /// One SSE frame, ready to yield verbatim.
    Frame(String),
    /// A reasoning frame plus its text, for the handler's own `ChatService` to persist.
    Reasoning { frame: String, block: String },
    /// The engine finished the turn, with whatever numbers it reported.
    TurnComplete {
        usage: Option<pond_core::models::ports::provider::UsageStats>,
        stats: Option<pond_core::shared::domain::turn_stats::TurnStats>,
    },
}

/// The one `tool_result` SSE frame shape; the MCP-UI renderer keys off the optional `ui`.
fn tool_result_frame(tool: &str, id: &str, content: &str, ui_hint: Option<Value>) -> String {
    let mut ev = json!({
        "type": "tool_result",
        "tool": tool,
        "id": id,
        "content": content,
    });
    if let Some(ui) = ui_hint {
        ev["ui"] = ui;
    }
    ev.to_string()
}

/// The one `turn_stats` SSE frame shape. `reasoning_tokens` is never deducted from
/// `completion_tokens`, and `None` stays `null`: "not counted" is not "zero".
fn turn_stats_frame(s: &pond_core::shared::domain::turn_stats::TurnStats) -> String {
    json!({
        "type": "turn_stats",
        "ttft_ms": s.ttft_ms,
        "prefill_ms": s.prefill_ms,
        "decode_tok_per_sec": s.decode_tok_per_sec,
        "prefill_tok_per_sec": s.prefill_tok_per_sec,
        "prefilled_tokens": s.prefilled_tokens,
        "reused_prefix_tokens": s.reused_prefix_tokens,
        "prompt_tokens": s.prompt_tokens,
        "completion_tokens": s.completion_tokens,
        "reasoning_tokens": s.reasoning_tokens,
        "context_used_tokens": s.context_used_tokens,
        "context_limit_tokens": s.context_limit_tokens,
        "context_pct": s.context_pct(),
        "model_load_ms": s.model_load_ms,
        "inference_count": s.inference_count,
        // Runs caused by thinking without answering, which `inference_count` lumps with tool runs.
        "reengagements": s.reengagements,
    })
    .to_string()
}

/// Per-turn state a chat SSE handler accumulates while the engine streams.
struct TurnAccumulator {
    /// Strips (and captures) Harmony thought channels and `<think>` blocks from visible text.
    thought: crate::thought_filter::ThoughtFilter,
    /// The assistant's visible answer, as persisted.
    full_text: String,
    /// One JSON row per tool result, as persisted.
    tool_results: Vec<String>,
    /// Tool-call id -> arguments, kept from `ToolCall` because `ToolResult` doesn't repeat them.
    tool_call_inputs: std::collections::HashMap<String, String>,
    /// First visible token time; fallback TTFT for engines that report none.
    ttft: Option<std::time::Instant>,
    tool_call_start: Option<std::time::Instant>,
    /// Last tool call's name and latency only: `TurnMetrics` has one slot.
    last_tool_name: Option<String>,
    last_tool_latency_ms: Option<u64>,
}

impl TurnAccumulator {
    /// Caller-built filter: the two routes capture thinking differently.
    fn new(thought: crate::thought_filter::ThoughtFilter) -> Self {
        Self {
            thought,
            full_text: String::new(),
            tool_results: Vec::new(),
            tool_call_inputs: std::collections::HashMap::new(),
            ttft: None,
            tool_call_start: None,
            last_tool_name: None,
            last_tool_latency_ms: None,
        }
    }

    /// Records timing for a Harmony-markup tool call, which emits no `AgentStreamEvent::ToolCall`.
    fn note_fallback_tool(&mut self, name: &str, latency: std::time::Duration) {
        self.last_tool_name = Some(name.to_string());
        self.last_tool_latency_ms = Some(latency.as_millis() as u64);
    }

    /// Folds one engine event into the turn and says what the stream should emit.
    /// The file's only match on `AgentStreamEvent`; `stream_handler_parity.rs` enforces that.
    fn absorb(&mut self, event: pond_core::models::ports::agent::AgentStreamEvent) -> StreamStep {
        use pond_core::models::ports::agent::AgentStreamEvent;

        match event {
            AgentStreamEvent::Status { content } => {
                StreamStep::Frame(json!({"type": "status", "content": content}).to_string())
            }
            AgentStreamEvent::Thinking { content } => StreamStep::Reasoning {
                frame: json!({"type": "thinking", "content": content}).to_string(),
                block: content,
            },
            AgentStreamEvent::ToolCall { tool, id, input } => {
                self.tool_call_start = Some(std::time::Instant::now());
                self.last_tool_name = Some(tool.clone());
                self.tool_call_inputs.insert(
                    id.clone(),
                    input
                        .as_ref()
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "{}".to_string()),
                );
                StreamStep::Frame(
                    json!({"type": "tool_call", "tool": tool, "id": id, "input": input})
                        .to_string(),
                )
            }
            AgentStreamEvent::ToolResult { tool, id, content } => {
                if let Some(start) = self.tool_call_start.take() {
                    self.last_tool_latency_ms = Some(start.elapsed().as_millis() as u64);
                }
                let (clean_content, ui_hint) = extract_ui_hint(&content);
                self.tool_results.push(
                    json!({
                        "tool_call_id": id,
                        "tool": tool,
                        "content": clean_content,
                        "arguments": self
                            .tool_call_inputs
                            .remove(&id)
                            .unwrap_or_else(|| "{}".to_string()),
                    })
                    .to_string(),
                );
                StreamStep::Frame(tool_result_frame(&tool, &id, &clean_content, ui_hint))
            }
            AgentStreamEvent::Text { content } => {
                let visible = self.thought.push(&content);
                if visible.is_empty() {
                    StreamStep::Nothing
                } else {
                    if self.ttft.is_none() {
                        self.ttft = Some(std::time::Instant::now());
                    }
                    self.full_text.push_str(&visible);
                    StreamStep::Frame(
                        json!({"type": "text", "content": visible, "token": visible}).to_string(),
                    )
                }
            }
            AgentStreamEvent::ReviewStatus { content } => {
                StreamStep::Frame(json!({"type": "review_status", "content": content}).to_string())
            }
            AgentStreamEvent::ReviewRevision {
                content,
                score,
                rounds,
            } => StreamStep::Frame(
                json!({"type": "review_revision", "content": content, "score": score, "rounds": rounds})
                    .to_string(),
            ),
            // Its own frame so the UI can offer a one-click continue.
            AgentStreamEvent::TurnLimitReached { max_turns } => StreamStep::Frame(
                json!({"type": "turn_limit_reached", "max_turns": max_turns}).to_string(),
            ),
            // Frame only: a subagent's activity never enters the parent's history; only its final
            // result does, as the `delegate` tool's ToolResult.
            AgentStreamEvent::SubagentProgress {
                task_id,
                role,
                status,
                detail,
            } => StreamStep::Frame(
                json!({
                    "type": "subagent_progress",
                    "task_id": task_id,
                    "role": role,
                    "status": status,
                    "detail": detail,
                })
                .to_string(),
            ),
            AgentStreamEvent::Done { usage, stats, .. } => StreamStep::TurnComplete { usage, stats },
            AgentStreamEvent::Error { content } => {
                StreamStep::Frame(json!({"error": content}).to_string())
            }
        }
    }
}

/// Ephemeral SSE turn for `run_recipe`, cancelled when its last reader leaves. `device` stays
/// apart from the client-controlled `req` so no body field can stand in for it.
fn chat_stream_inner(
    state: Arc<AppState>,
    permit: tokio::sync::OwnedSemaphorePermit,
    req: ChatRequest,
    device: ProvenDevice,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let run = new_run(&req, &device, crate::runs::RunPolicy::Ephemeral);
    spawn_run(state, permit, run, None, req, device)
}

/// Builds a run handle, minting the session id here: the registry indexes runs by session.
fn new_run(
    req: &ChatRequest,
    device: &ProvenDevice,
    policy: crate::runs::RunPolicy,
) -> Arc<crate::runs::RunHandle> {
    let session_id = req
        .session_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let owner = match device.id() {
        Some(id) => crate::runs::RunOwner::Device(id.to_string()),
        None => crate::runs::RunOwner::Unattributed,
    };
    crate::runs::RunHandle::new(session_id, owner, policy)
}

/// Starts the turn as an owned task and returns an SSE body attached to it.
/// `run_permit` lives as long as the task; `permit` (`sse_semaphore`) only while someone reads.
fn spawn_run(
    state: Arc<AppState>,
    permit: tokio::sync::OwnedSemaphorePermit,
    run: Arc<crate::runs::RunHandle>,
    run_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    req: ChatRequest,
    device: ProvenDevice,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    tracing::info!(
        target: "giap::runs",
        run_id = %run.run_id,
        session_id = %run.session_id,
        policy = ?run.policy,
        "agent run spawned"
    );
    tokio::spawn(run_turn(
        state.clone(),
        run.clone(),
        run_permit,
        req,
        device,
    ));
    attach_sse(state, permit, run, 0, AttachKind::Original)
}

/// One agent turn, start to finish, whether or not anybody is listening.
/// `stream_handler_parity.rs` pins the order: scope before `ChatService`, persist before `done`.
async fn run_turn(
    state: Arc<AppState>,
    run: Arc<crate::runs::RunHandle>,
    run_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    req: ChatRequest,
    device: ProvenDevice,
) {
    // Released when the task ends, not when a reader goes away.
    let _run_permit = run_permit;
    drive_turn(&state, &run, req, device).await;
    // Re-stamp now the turn is over: idle work (summaries, consolidation) must measure from a
    // turn's end, not its start, or a long turn looks idle mid-generation.
    state.note_user_activity().await;
    // `finish` keeps the first terminal state, so an earlier cancel is not relabelled.
    run.finish(crate::runs::RunState::Finished);
    tracing::info!(
        target: "giap::runs",
        run_id = %run.run_id,
        state = ?run.state(),
        last_seq = run.last_seq(),
        "agent run finished"
    );
}

async fn drive_turn(
    state: &Arc<AppState>,
    run: &Arc<crate::runs::RunHandle>,
    req: ChatRequest,
    device: ProvenDevice,
) {
    use futures::StreamExt;

    let turn_start = std::time::Instant::now();
    let session_id = run.session_id.clone();
    let storage = &state.session_storage;

    if storage.get_session(&session_id).await.is_err() {
        if let Err(e) = storage.create_session(session_id.clone()).await {
            let data = json!({"error": format!("Failed to create session: {}", e)}).to_string();
            run.push(data, true);
            run.finish(crate::runs::RunState::Failed);
            return;
        }
    }

    let settings = state.settings_repo.get().await.unwrap_or_default();

    // The adapter builds the system prompt, so `AppState::prompt_template_dir` and
    // `AppState::mcp_memory` are inert until rewired.

    let model_role = "chat";

    // Shared with the AgentRequest so memory is written under the identity it was read under.
    let turn_scope = resolve_turn_scope(state, &session_id, &device).await;

    let mut chat_service = pond_core::shared::services::chat::ChatService::new(
        state.agent.clone(),
        session_id.clone(),
        storage.clone(),
    )
    .with_profile_scope(turn_scope.clone())
    // Gates `record_thinking`, which the stream calls unconditionally; off by default.
    .with_thinking(settings.persist_thinking);
    if let (Some(ext), Some(svc)) = (
        state.memory_extractor.clone(),
        state.memory_extraction_service.clone(),
    ) {
        chat_service = chat_service.with_memory_extraction(ext, svc, state.memory_repo.clone());
    }
    if let Some(event_log) = state.event_log.clone() {
        chat_service = chat_service.with_event_log(event_log);
    }

    // ── Persist user message ────────────────────────────────────────────
    // Images persist with the message so later turns still see them after a trim or restart.
    // The id is kept for the repair below: this row lands before inference starts.
    let user_message_id = match chat_service
        .persist_user_message_with_images(&req.message, req.images.clone())
        .await
    {
        Ok(id) => id,
        Err(e) => {
            let data =
                json!({"error": format!("Failed to persist user message: {}", e)}).to_string();
            run.push(data, true);
            run.finish(crate::runs::RunState::Failed);
            return;
        }
    };

    // ── On-demand llamafile startup ─────────────────────────────────────
    {
        let is_llamafile_role = settings.chat_provider == "llamafile";

        if is_llamafile_role {
            if let Some(manager) = &state.llamafile_manager {
                if !manager.is_running().await {
                    let status =
                        json!({"type": "status", "content": "Model starting…"}).to_string();
                    run.push(status, false);

                    let model_hint = if settings.chat_provider == "llamafile" {
                        Some(settings.chat_model.as_str())
                    } else {
                        None
                    };

                    let (_url, ready) = manager.ensure_started_and_wait(model_hint, 90).await;

                    if !ready {
                        let data = json!({"error":
                            "llamafile did not start within 90 s — \
                             check that a model file is installed"
                        })
                        .to_string();
                        run.push(data, true);
                        run.finish(crate::runs::RunState::Failed);
                        return;
                    }
                }
            }
        }
    }

    let mut usage_prompt_tokens: u32 = 0;
    let mut usage_completion_tokens: u32 = 0;
    let mut turn_stats: Option<pond_core::shared::domain::turn_stats::TurnStats> = None;

    let model_name_for_done = settings.chat_model.clone();

    use pond_core::shared::domain::agent::AgentRequest;

    let agent_req = AgentRequest {
        message: req.message.clone(),
        session_id: session_id.clone(),
        model_role: model_role.to_string(),
        images: req.images.clone(),
        voice_mode: req.voice_mode,
        canvas_mode: req.canvas_mode,
        profile_scope: turn_scope.clone(),
        profile_context: profile_context_for(state, &turn_scope).await,
        tool_group_allowlist: req.tool_group_allowlist.clone(),
        warmup: false,
    };

    let mut turn = TurnAccumulator::new(if settings.show_thinking && !req.voice_mode {
        crate::thought_filter::ThoughtFilter::new().with_thinking_capture()
    } else {
        crate::thought_filter::ThoughtFilter::new()
    });
    let mut agent_stream = match state.agent.chat_stream(agent_req).await {
        Ok(s) => s,
        Err(e) => {
            let data = json!({"error": e.to_string()}).to_string();
            run.push(data, true);
            run.finish(crate::runs::RunState::Failed);
            return;
        }
    };

    // ── Agent turn idle timeout ────────────────────────────────────
    // Bounds silence between events, not total generation; 0 disables it (24 h sentinel).
    let timeout_secs = settings.agent_timeout_secs;
    let idle_budget = if timeout_secs == 0 {
        std::time::Duration::from_secs(86_400)
    } else {
        std::time::Duration::from_secs(timeout_secs)
    };
    let mut deadline = tokio::time::Instant::now() + idle_budget;
    let mut timed_out = false;
    let mut cancelled = false;

    loop {
        // `biased`: a cancel must beat already-queued events, or a voice barge-in isn't one.
        let next = tokio::select! {
            biased;
            _ = run.cancel.cancelled() => {
                cancelled = true;
                tracing::info!(
                    target: "giap::runs",
                    run_id = %run.run_id,
                    session_id = %session_id,
                    "agent run cancelled; stopping the turn"
                );
                break;
            }
            next = tokio::time::timeout_at(deadline, agent_stream.next()) => next,
        };
        match next {
            Ok(Some(event_result)) => {
                deadline = tokio::time::Instant::now() + idle_budget;
                match event_result {
                    Ok(event) => {
                        match turn.absorb(event) {
                            StreamStep::Nothing => {}
                            StreamStep::Frame(data) => {
                                run.push(data, false);
                            }
                            StreamStep::Reasoning { frame, block } => {
                                // Showing and keeping thinking are separate consents.
                                chat_service.record_thinking(block);
                                run.push(frame, false);
                            }
                            StreamStep::TurnComplete { usage, stats } => {
                                if let Some(u) = usage {
                                    usage_prompt_tokens = u.prompt_tokens;
                                    usage_completion_tokens = u.completion_tokens;
                                }
                                if let Some(s) = stats {
                                    let payload = turn_stats_frame(&s);
                                    turn_stats = Some(s);
                                    run.push(payload, false);
                                }
                                // This route's `done` frame comes last, after persistence.
                                continue;
                            }
                        }
                        for thinking_content in turn.thought.take_thinking() {
                            let data = json!({"type": "thinking", "content": thinking_content})
                                .to_string();
                            run.push(data, false);
                        }
                        // Fallback: execute tool calls the model wrote as Harmony text markup.
                        for body in turn.thought.take_tool_calls() {
                            if let Some((name, args)) =
                                crate::thought_filter::parse_tool_envelope(&body)
                            {
                                tracing::info!(tool = %name, "Executing text-based tool call (model used Harmony format)");
                                let call_id = uuid::Uuid::new_v4().to_string();
                                let args_val: serde_json::Value =
                                    serde_json::from_str(&args).unwrap_or(json!({}));
                                run.push(
                                    json!({"type": "tool_call", "tool": name.clone(), "id": call_id.clone(), "input": args_val}).to_string(),
                                    false,
                                );
                                let call_start = std::time::Instant::now();
                                let call_result =
                                    state.agent.call_tool(&session_id, &name, &args).await;
                                turn.note_fallback_tool(&name, call_start.elapsed());
                                match call_result {
                                    Ok(result_text) => {
                                        let (clean, ui_hint) = extract_ui_hint(&result_text);
                                        run.push(
                                            tool_result_frame(&name, &call_id, &clean, ui_hint),
                                            false,
                                        );
                                    }
                                    Err(e) => {
                                        run.push(
                                            tool_result_frame(
                                                &name,
                                                &call_id,
                                                &format!("Tool error: {e}"),
                                                None,
                                            ),
                                            false,
                                        );
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
                        run.push(data, true);
                        run.finish(crate::runs::RunState::Failed);
                        return;
                    }
                }
            }
            Ok(None) => {
                break;
            }
            Err(_elapsed) => {
                timed_out = true;
                tracing::warn!(
                    session_id = %session_id,
                    timeout_secs = timeout_secs,
                    "Agent turn timed out"
                );
                let data = json!({"error": "Agent timed out. Try a shorter message or start a new session."}).to_string();
                run.push(data, false);
                break;
            }
        }
    }

    // Must drop here: it fires the adapter's `DropGuard`, cancelling the loop and releasing the
    // lease and device claim, which the review's model call below must not hold.
    drop(agent_stream);

    if !timed_out && !cancelled {
        let tail = turn.thought.flush();
        if !tail.is_empty() {
            turn.full_text.push_str(&tail);
            let data = json!({"type": "text", "content": tail, "token": tail}).to_string();
            run.push(data, false);
        }
    }

    // ── Adversarial answer review (post-inference) ───────────────
    // A `review_revision` frame tells the frontend to replace the streamed text.
    {
        let should_review = !timed_out
            && !cancelled
            && match settings.review_mode.as_str() {
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
                let status =
                    json!({"type": "review_status", "content": "Reviewing answer..."}).to_string();
                run.push(status, false);

                match reviewer.review(&req.message, &turn.full_text, None).await {
                    Ok(result) if result.was_revised => {
                        turn.full_text = result.final_answer.clone();
                        let data = json!({
                            "type": "review_revision",
                            "content": result.final_answer,
                            "score": result.verdict.score,
                            "rounds": result.rounds,
                        })
                        .to_string();
                        run.push(data, false);
                    }
                    Ok(result) => {
                        let status = json!({
                            "type": "review_status",
                            "content": format!("Answer verified (score: {}/5)", result.verdict.score),
                        }).to_string();
                        run.push(status, false);
                    }
                    Err(e) => {
                        tracing::warn!("Answer review failed (non-fatal): {}", e);
                    }
                }
            }
        }
    }

    // ── Persist assistant turn + memory extraction ────────────────────
    // `persist_assistant_turn_with_extraction` also spawns memory extraction. A cancel before
    // any output removes the orphaned user message; any output, or a timeout, keeps both.
    let said_nothing = turn.full_text.trim().is_empty() && turn.tool_results.is_empty();
    if cancelled && said_nothing {
        match state
            .session_storage
            .delete_messages_from(&session_id, &user_message_id)
            .await
        {
            Ok(()) => tracing::info!(
                target: "giap::runs",
                run_id = %run.run_id,
                session_id = %session_id,
                user_message_id = %user_message_id,
                "run ended before it said anything; removed the orphaned user message"
            ),
            Err(e) => tracing::warn!(
                target: "giap::runs",
                run_id = %run.run_id,
                session_id = %session_id,
                error = %e,
                "could not remove the orphaned user message"
            ),
        }
    } else {
        let _ = chat_service
            .persist_assistant_turn_with_extraction(
                std::mem::take(&mut turn.tool_results),
                &turn.full_text,
                Some((usage_prompt_tokens, usage_completion_tokens)),
                Some(&model_name_for_done),
                &req.message,
            )
            .await;
    }

    // Resolved once so telemetry and the growth monitor agree. The catalog row (rung 3) is the
    // one the Models tab shows; a missing repo or row falls through to lower rungs.
    let catalog_context_length = match &state.model_repo {
        Some(repo) => {
            let id = ModelRecord::id_for(
                &ModelCategory::for_chat_provider(&settings.chat_provider),
                &settings.chat_model,
            );
            repo.get_by_id(&id)
                .await
                .ok()
                .flatten()
                .and_then(|m| m.context_length)
        }
        None => None,
    };

    let turn_context_limit = ContextGovernor::resolve(&ContextInputs {
        provider: &settings.chat_provider,
        model: &settings.chat_model,
        override_tokens: settings.context_window_override,
        // The adapter owns the registry lookup; its result arrives via TurnStats.
        registry_pinned: None,
        catalog_context_length,
        engine_reported: turn_stats
            .as_ref()
            .and_then(|s| s.context_limit_tokens)
            .map(|t| EngineWindow::new(settings.chat_model.clone(), t)),
        capability_window: Some(state.agent.capabilities().context_window_tokens),
    })
    .tokens as u32;

    // ── Per-turn telemetry ──────────────────────────────────────────
    if settings.telemetry_enabled {
        if let Some(ref telemetry) = state.telemetry {
            let total_latency_ms = turn_start.elapsed().as_millis() as u64;
            // Prefer the engine's own TTFT; fall back to first-SSE-text time.
            let ttft_ms = turn_stats
                .as_ref()
                .and_then(|s| s.ttft_ms)
                .or_else(|| {
                    turn.ttft
                        .map(|t| t.duration_since(turn_start).as_millis() as u64)
                })
                .unwrap_or(total_latency_ms);

            let existing_turns = telemetry
                .get_turns(&session_id)
                .await
                .map(|v| v.len() as u32)
                .unwrap_or(0);

            let context_limit = turn_context_limit;
            let context_used = turn_stats
                .as_ref()
                .and_then(|s| s.context_used_tokens)
                .unwrap_or(usage_prompt_tokens + usage_completion_tokens);
            let context_utilization_pct = if context_limit > 0 {
                (context_used as f32 / context_limit as f32) * 100.0
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
                tool_name: turn.last_tool_name.clone(),
                tool_latency_ms: turn.last_tool_latency_ms,
                tool_cache_hit: None,
                context_utilization_pct,
                model_name: model_name_for_done.clone(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                prefill_ms: turn_stats.as_ref().and_then(|s| s.prefill_ms),
                model_load_ms: turn_stats.as_ref().and_then(|s| s.model_load_ms),
                decode_tok_per_sec: turn_stats.as_ref().and_then(|s| s.decode_tok_per_sec),
                prefill_tok_per_sec: turn_stats.as_ref().and_then(|s| s.prefill_tok_per_sec),
                prefilled_tokens: turn_stats.as_ref().map(|s| s.prefilled_tokens),
                reused_prefix_tokens: turn_stats.as_ref().and_then(|s| s.reused_prefix_tokens),
                context_limit_tokens: turn_stats.as_ref().and_then(|s| s.context_limit_tokens),
                inference_count: turn_stats.as_ref().map(|s| s.inference_count),
                // None = not measured, which differs from zero; don't collapse to 0.
                reasoning_tokens: turn_stats.as_ref().and_then(|s| s.reasoning_tokens),
                reengagements: turn_stats.as_ref().map(|s| s.reengagements),
            };

            if let Err(e) = telemetry.record_turn(metrics).await {
                tracing::debug!(target: "giap::telemetry", "failed to record turn metrics: {e}");
            }
        }
    }

    // ── Context growth monitoring ─────────────────────────────────
    if settings.context_monitor_enabled {
        let estimated_tokens = usage_prompt_tokens + usage_completion_tokens;
        let context_limit = turn_context_limit;

        if estimated_tokens > 0 && context_limit > 0 {
            state
                .context_monitor
                .record_turn(&session_id, estimated_tokens, context_limit);

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
                })
                .to_string();
                run.push(data, false);
            }
        }
    }

    // Set the terminal state before the final frame, so the discovery route and frame agree.
    if cancelled {
        run.finish(crate::runs::RunState::Cancelled);
        let data = json!({
            "type": "cancelled",
            "run_id": run.run_id,
            "at_seq": run.last_seq(),
        })
        .to_string();
        run.push(data, false);
    } else if timed_out {
        run.finish(crate::runs::RunState::Failed);
    }

    let data = json!({
        "done": true,
        "interrupted": cancelled || timed_out,
        "run_id": run.run_id,
        "session_id": session_id,
        "model_role": model_role,
        "model_name": model_name_for_done,
        "usage": {
            "prompt_tokens": usage_prompt_tokens,
            "completion_tokens": usage_completion_tokens,
        }
    })
    .to_string();
    run.push(data, true);
}

/// Whether this subscriber started the turn or came back to it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AttachKind {
    Original,
    Reattach,
}

/// One subscriber: replay what it missed, then follow the live tail. Subscribe before
/// snapshotting, or a frame produced in between is missed by both.
fn attach_sse(
    state: Arc<AppState>,
    permit: tokio::sync::OwnedSemaphorePermit,
    run: Arc<crate::runs::RunHandle>,
    after_seq: u64,
    kind: AttachKind,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let guard = run.attach();
    let mut rx = run.subscribe();
    let snapshot = run.snapshot(after_seq);
    let epoch = state.runs.epoch.clone();

    tracing::info!(
        target: "giap::runs",
        run_id = %run.run_id,
        kind = ?kind,
        from_seq = after_seq,
        replay_depth = snapshot.frames.len(),
        gap = ?snapshot.gap,
        state = ?snapshot.state,
        "client attached to run"
    );

    let stream = async_stream::stream! {
        // Held only while somebody is reading.
        let _permit = permit;
        let _guard = guard;

        if kind == AttachKind::Reattach {
            let data = json!({
                "type": "reattached",
                "run_id": run.run_id,
                "session_id": run.session_id,
                "from_seq": after_seq,
                "replay_depth": snapshot.frames.len(),
                "state": snapshot.state,
                "epoch": epoch,
            }).to_string();
            yield Ok(Event::default().data(data));
        }

        // The ring rolled past this client: frames are lost, only a session reload recovers.
        if let Some(first_available) = snapshot.gap {
            let data = json!({
                "type": "replay_gap",
                "requested_after_seq": after_seq,
                "first_available_seq": first_available,
                "advice": "reload_session_messages",
            }).to_string();
            yield Ok(Event::default().data(data));
        }

        let mut sent = after_seq;
        let saw_terminal = snapshot.saw_terminal;
        for frame in snapshot.frames {
            sent = frame.seq;
            yield Ok(Event::default().id(frame.seq.to_string()).data(&*frame.payload));
        }
        if saw_terminal {
            return;
        }

        loop {
            match rx.recv().await {
                // Already replayed from the snapshot.
                Ok(frame) if frame.seq <= sent => continue,
                Ok(frame) => {
                    let terminal = frame.terminal;
                    sent = frame.seq;
                    yield Ok(Event::default().id(frame.seq.to_string()).data(&*frame.payload));
                    if terminal {
                        break;
                    }
                }
                // The ring outlasts the broadcast queue, so missed frames are re-read from it.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        target: "giap::runs",
                        run_id = %run.run_id,
                        lagged = n,
                        from_seq = sent,
                        "attached client lagged; recovering from the replay buffer"
                    );
                    let recovered = run.snapshot(sent);
                    if let Some(first_available) = recovered.gap {
                        let data = json!({
                            "type": "replay_gap",
                            "requested_after_seq": sent,
                            "first_available_seq": first_available,
                            "advice": "reload_session_messages",
                        }).to_string();
                        yield Ok(Event::default().data(data));
                    }
                    let mut done = false;
                    for frame in recovered.frames {
                        sent = frame.seq;
                        done = frame.terminal;
                        yield Ok(Event::default().id(frame.seq.to_string()).data(&*frame.payload));
                        if done {
                            break;
                        }
                    }
                    if done {
                        break;
                    }
                }
                // The handle was dropped under an attached client; say so rather than go silent.
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tracing::warn!(
                        target: "giap::runs",
                        run_id = %run.run_id,
                        at_seq = sent,
                        "run dropped while a client was attached"
                    );
                    let data = json!({
                        "type": "run_evicted",
                        "run_id": run.run_id,
                        "advice": "reload_session_messages",
                    }).to_string();
                    yield Ok(Event::default().data(data));
                    break;
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ── Reattaching to a run in flight ────────────────────────────────────────────

/// May this caller follow this run? Only the device that started it: frames can be private.
fn may_reattach(run: &crate::runs::RunHandle, device: &ProvenDevice) -> bool {
    match &run.owner {
        crate::runs::RunOwner::Device(owner) => device.id() == Some(owner.as_str()),
        crate::runs::RunOwner::Unattributed => true,
    }
}

/// Finds the run; unknown and not-yours are the same 404, so run ids can't be probed.
fn lookup_run(
    state: &AppState,
    run_id: &str,
    device: &ProvenDevice,
) -> Result<Arc<crate::runs::RunHandle>, (StatusCode, Json<Value>)> {
    let not_found = || (StatusCode::NOT_FOUND, Json(json!({"error": "unknown run"})));
    let run = state.runs.registry.get(run_id).ok_or_else(not_found)?;
    if !may_reattach(&run, device) {
        tracing::warn!(
            target: "giap::runs",
            run_id = %run_id,
            asking_device = ?device.id(),
            "refused a reattach: the run belongs to another device"
        );
        return Err(not_found());
    }
    Ok(run)
}

#[derive(serde::Deserialize)]
struct ReattachQuery {
    /// Exclusive. Absent means "from the beginning".
    #[serde(default)]
    after_seq: Option<u64>,
    /// The epoch the client was told when the run started.
    #[serde(default)]
    epoch: Option<String>,
}

/// What a client that only knows its session id needs to find its way back.
async fn session_active_run(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let device = proven_device(principal.as_ref());
    let run = state
        .runs
        .registry
        .for_session(&session_id)
        .filter(|r| may_reattach(r, &device))
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "no active run for this session"})),
            )
        })?;
    Ok(Json(json!({
        "run_id": run.run_id,
        "session_id": run.session_id,
        "state": run.state(),
        "started_at": run.started_at.to_rfc3339(),
        "first_seq": run.first_seq(),
        "last_seq": run.last_seq(),
        "epoch": state.runs.epoch,
    })))
}

/// Follow a run that is already in flight, or replay one that just finished.
async fn reattach_run_events(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    Path(run_id): Path<String>,
    Query(q): Query<ReattachQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)>
{
    let device = proven_device(principal.as_ref());

    // Another epoch's run died with the last process: 410, distinguishable from an aged-out 404.
    if let Some(epoch) = q.epoch.as_deref() {
        if epoch != state.runs.epoch {
            return Err((
                StatusCode::GONE,
                Json(json!({
                    "error": "run_lost",
                    "reason": "server_restarted",
                    "epoch": state.runs.epoch,
                    "advice": "reload_session_messages",
                })),
            ));
        }
    }

    let run = lookup_run(&state, &run_id, &device)?;

    // Browsers resend `Last-Event-ID` themselves; `after_seq` survives a restart. The larger wins.
    let from_header = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let after_seq = q.after_seq.unwrap_or(0).max(from_header.unwrap_or(0));

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

    Ok(attach_sse(
        state,
        permit,
        run,
        after_seq,
        AttachKind::Reattach,
    ))
}

/// Stops a run (dropping the connection doesn't stop a detached one). Idempotent.
async fn cancel_run(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let device = proven_device(principal.as_ref());
    let run = lookup_run(&state, &run_id, &device)?;
    cancel_handle(&run, device.id());
    Ok(Json(json!({
        "run_id": run.run_id,
        "state": run.state(),
        "at_seq": run.last_seq(),
    })))
}

/// The same stop, for a caller that knows the session rather than the run.
async fn cancel_session_run(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let device = proven_device(principal.as_ref());
    let run = state
        .runs
        .registry
        .for_session(&session_id)
        .filter(|r| may_reattach(r, &device))
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "no active run for this session"})),
            )
        })?;
    cancel_handle(&run, device.id());
    Ok(Json(json!({
        "run_id": run.run_id,
        "state": run.state(),
        "at_seq": run.last_seq(),
    })))
}

/// Fires the token; never `JoinHandle::abort()`, which would skip the persistence tail.
fn cancel_handle(run: &crate::runs::RunHandle, requested_by: Option<&str>) {
    if run.state().is_terminal() {
        return;
    }
    tracing::info!(
        target: "giap::runs",
        run_id = %run.run_id,
        at_seq = run.last_seq(),
        requested_by = ?requested_by,
        "run cancelled on request"
    );
    run.cancel.cancel();
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
        let message_count = state
            .session_storage
            .count_messages(&s.id)
            .await
            .unwrap_or(0);

        // Untitled sessions get a read-time label from the first user message; nothing is stored.
        let effective_title: Option<String> = match &s.title {
            Some(t) if !t.trim().is_empty() => Some(t.clone()),
            _ => state
                .session_storage
                .first_user_message(&s.id)
                .await
                .ok()
                .flatten()
                .as_deref()
                .map(derived_session_label)
                .filter(|t| !t.is_empty()),
        };

        let preview: Option<String> = state
            .session_storage
            .first_assistant_message(&s.id)
            .await
            .ok()
            .flatten()
            .as_deref()
            .map(session_preview)
            .filter(|p| !p.is_empty());

        session_list.push(json!({
            "id": s.id,
            "title": effective_title,
            "preview": preview,
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

/// Short label from a session's first user message, for the untitled-session fallback.
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

/// The pond's first answer, for a history card (the title already carries the question).
/// Capped past any card's width so the client fades the overflow instead of an ellipsis.
fn session_preview(text: &str) -> String {
    const MAX_CHARS: usize = 240;
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = cleaned.trim();
    if cleaned.chars().count() <= MAX_CHARS {
        return cleaned.to_string();
    }
    let truncated: String = cleaned.chars().take(MAX_CHARS).collect();
    format!("{}…", truncated.trim_end())
}

/// `GET /api/v1/usage/summary` — aggregate token usage across all sessions.
/// `counted_reasoning_turns` tells "no thinking" from "not counted" (HTTP turns store NULL).
/// TODO: sum reasoning with one `SessionStorage` query, not an O(corpus) message walk.
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

    let mut total_reasoning: u64 = 0;
    let mut counted_reasoning_turns: u64 = 0;
    for session in &sessions {
        // An unreadable session contributes nothing rather than failing the whole summary.
        match state.session_storage.get_messages(&session.id).await {
            Ok(messages) => {
                for reasoning in messages.iter().filter_map(|m| m.reasoning_tokens) {
                    total_reasoning += reasoning as u64;
                    counted_reasoning_turns += 1;
                }
            }
            Err(e) => tracing::warn!(
                error = %e,
                session_id = %session.id,
                "could not read a session's messages while summing reasoning tokens"
            ),
        }
    }

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
        // Not in `total_tokens`, which prices are applied to: this is GIAP's estimate, not billed.
        "total_reasoning_tokens": total_reasoning,
        "counted_reasoning_turns": counted_reasoning_turns,
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

/// Deletes a session and its messages; idempotent (a missing session also gets 204).
async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    use pond_core::user_data::ports::session_storage::SessionStorageError;

    // Must run before the row delete: that cascades away the engine pairing, orphaning the
    // engine-side session for good.
    state.agent.forget_session(&session_id).await;

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

    // Otherwise the growth map leaks, and a recycled id would inherit this session's state.
    state.context_monitor.reset_session(&session_id);

    Ok(StatusCode::NO_CONTENT)
}

/// Deletes a message and every later one: the edit/refresh primitive (client then resubmits).
async fn delete_messages_from_handler(
    State(state): State<Arc<AppState>>,
    Path((session_id, message_id)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    use pond_core::user_data::ports::session_storage::SessionStorageError;
    state
        .session_storage
        .delete_messages_from(&session_id, &message_id)
        .await
        .map_err(|e| {
            let status = match &e {
                SessionStorageError::SessionNotFound(_)
                | SessionStorageError::MessageNotFound(_) => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({"error": format!("{}", e)})))
        })?;

    // The engine still holds the deleted turns; dropping the pairing makes the next turn
    // hydrate a fresh engine session from the truncated pond history.
    state.agent.forget_session(&session_id).await;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MessageFeedbackRequest {
    /// `true` = liked (training data), `false` = disliked (excluded), `null` = clear the vote.
    #[serde(default)]
    liked: Option<bool>,
}

/// Set or clear the like/dislike training-feedback flag on one message.
async fn set_message_feedback_handler(
    State(state): State<Arc<AppState>>,
    Path((session_id, message_id)): Path<(String, String)>,
    body: Result<Json<MessageFeedbackRequest>, JsonRejection>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    use pond_core::user_data::ports::session_storage::SessionStorageError;
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;

    state
        .session_storage
        .set_message_feedback(&session_id, &message_id, req.liked)
        .await
        .map_err(|e| {
            let status = match &e {
                SessionStorageError::SessionNotFound(_)
                | SessionStorageError::MessageNotFound(_) => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({"error": format!("{}", e)})))
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Session messages: the newest `limit` (≤500) by default, or oldest-first from `offset`.
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

    // An absent `offset` means the newest page, which `offset=0` (the oldest page) is not.
    let messages = if params.contains_key("offset") {
        state
            .session_storage
            .get_messages_paginated(&session_id, limit, offset)
            .await
    } else {
        state
            .session_storage
            .get_recent_messages(&session_id, limit)
            .await
    }
    .map_err(|e| {
        let status = match &e {
            SessionStorageError::SessionNotFound(_) => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(json!({"error": format!("{}", e)})))
    })?;

    // Metadata only (no bytes); a storage error must not fail a history read.
    let attachments_by_message: std::collections::HashMap<
        String,
        Vec<pond_core::user_data::domain::session::MessageAttachment>,
    > = state
        .session_storage
        .list_session_attachments(&session_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .fold(std::collections::HashMap::new(), |mut acc, a| {
            acc.entry(a.message_id.clone()).or_default().push(a);
            acc
        });

    // The only production caller of `get_thinking_for_session` (enforced by pond-core's
    // `thinking_is_never_replayed.rs`): it goes to the HTTP response, never to the model.
    let thinking_by_message: std::collections::HashMap<String, Vec<String>> = state
        .session_storage
        .get_thinking_for_session(&session_id)
        .await
        .unwrap_or_default();

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
                "liked": m.liked,
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
            // Referenced by URL, never inlined: base64 would make a history read megabytes.
            if let Some(atts) = attachments_by_message.get(&m.id) {
                obj["images"] = json!(atts
                    .iter()
                    .map(|a| json!({
                        "id": a.id,
                        "mime_type": a.mime_type,
                        "byte_size": a.byte_size,
                        "url": format!(
                            "/api/v1/sessions/{}/attachments/{}",
                            urlencoding_lite(&a.session_id),
                            urlencoding_lite(&a.id),
                        ),
                    }))
                    .collect::<Vec<_>>());
            }
            // Absent, not `[]`, when none was kept: "not kept" differs from "kept nothing".
            if let Some(blocks) = thinking_by_message.get(&m.id) {
                obj["thinking"] = json!(blocks);
            }
            obj
        })
        .collect();

    Ok(Json(json!({ "messages": list })))
}

/// The body every outcome of `POST /sessions/:id/compact` reports; `context` mirrors the
/// `context_warning` frame, except unknown `turns_remaining` is `null`, not `u32::MAX`.
fn compaction_report(
    session_id: &str,
    status: &str,
    reason: Option<&str>,
    outcome: Option<&str>,
    health: &ContextHealth,
) -> Json<Value> {
    Json(json!({
        "session_id": session_id,
        "status": status,
        "reason": reason,
        "outcome": outcome,
        "context": {
            "utilization_pct": health.utilization_pct,
            "turns_remaining": (health.estimated_turns_remaining != u32::MAX)
                .then_some(health.estimated_turns_remaining),
            "avg_growth_rate": health.avg_growth_rate,
            "should_compact": health.should_compact,
            "warning": health.warning,
        },
    }))
}

/// POST /api/v1/sessions/:session_id/compact — compact this session now, via the engine.
/// Non-fault outcomes are 200 + `status`/`reason`; 404 only for an unknown session id.
async fn compact_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state
        .session_storage
        .get_session(&session_id)
        .await
        .map_err(|e| {
            let status = match &e {
                SessionStorageError::SessionNotFound(_) => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({ "error": format!("{e}") })))
        })?;

    let settings = state.settings_repo.get().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("read settings: {e}") })),
        )
    })?;

    let health = state.context_monitor.check_context_health(&session_id);

    // In this order: only the first reason that applies is worth reporting.
    if !settings.context_monitor_enabled {
        // Nothing recorded any turn, so utilisation zeros here mean "not measured".
        return Ok(compaction_report(
            &session_id,
            "skipped",
            Some("monitor_disabled"),
            None,
            &health,
        ));
    }
    // Deliberately not gated on `hybrid_compaction_enabled`; the engine ignores that setting.
    if !health.should_compact {
        return Ok(compaction_report(
            &session_id,
            "skipped",
            Some("not_under_pressure"),
            None,
            &health,
        ));
    }

    // Runs goose's `compact_messages` (manual); goose serialises compaction per session itself.
    let (status, reason, outcome_label) = match state.agent.compact_session(&session_id).await {
        Ok(Some(_retained)) => {
            state.context_monitor.note_compacted(&session_id);
            ("compacted", None, Some("refreshed"))
        }
        // No manual compaction in this backend, or nothing to compact.
        Ok(None) => (
            "skipped",
            Some("nothing_to_summarise"),
            Some("nothing_to_do"),
        ),
        Err(e) => {
            tracing::warn!("manual compaction failed for {session_id}: {e}");
            ("skipped", Some("failed"), None)
        }
    };

    // Re-read: `note_compacted` drops growth samples, so `turns_remaining` is `null` after.
    let after = state.context_monitor.check_context_health(&session_id);
    Ok(compaction_report(
        &session_id,
        status,
        reason,
        outcome_label,
        &after,
    ))
}

/// POST /api/v1/sessions/retitle — rename conversations now, without waiting for idle.
/// Skips only the scheduling gate and `session_titling_enabled`; per-conversation rules hold.
async fn retitle_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    use pond_core::shared::domain::session_activity::SessionOrigin;
    use pond_core::shared::services::session_title::{
        RetitleOutcome, SessionTitleService, SkipReason,
    };

    // Each rename is a model call; `capped` tells the caller to press again for the rest.
    const MANUAL_MAX_RENAMES: usize = 20;

    let Some(provider) = state.llm_provider.read().await.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "No language model is configured" })),
        ));
    };

    let sessions = state.session_storage.list_sessions().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("list sessions: {e}") })),
        )
    })?;

    let service = SessionTitleService::new(provider, state.session_storage.clone());
    // Never cancelled: this run was asked for, so activity must not cut it short.
    let cancel = tokio_util::sync::CancellationToken::new();

    let mut renamed: Vec<Value> = Vec::new();
    let mut considered = 0usize;
    let mut unusable = 0usize;
    let mut failed = 0usize;
    let (mut user_named, mut still_current, mut too_short, mut unknown) = (0, 0, 0, 0);
    let mut capped = false;

    for session in sessions {
        // Nobody browses the pond's own background sessions.
        if !SessionOrigin::of(&session.id).is_human() {
            continue;
        }
        if renamed.len() >= MANUAL_MAX_RENAMES {
            capped = true;
            break;
        }
        considered += 1;

        match service.retitle(&session.id, &cancel).await {
            Ok(RetitleOutcome::Retitled { title, .. }) => {
                tracing::info!(
                    target: "giap::trace",
                    kind = "session_retitled",
                    trigger = "manual",
                    session_id = %session.id,
                    title = %title,
                );
                renamed.push(json!({ "session_id": session.id, "title": title }));
            }
            Ok(RetitleOutcome::Skipped(reason)) => match reason {
                SkipReason::UserNamed => user_named += 1,
                SkipReason::StillCurrent => still_current += 1,
                SkipReason::TooShort => too_short += 1,
                SkipReason::UnknownProvenance => unknown += 1,
            },
            Ok(RetitleOutcome::Unusable) => unusable += 1,
            Ok(RetitleOutcome::Cancelled) => {}
            Err(e) => {
                tracing::debug!("manual re-title of {} failed: {e}", session.id);
                failed += 1;
            }
        }
    }

    Ok(Json(json!({
        "renamed": renamed,
        "renamed_count": renamed.len(),
        "considered": considered,
        "capped": capped,
        "unusable": unusable,
        "failed": failed,
        "skipped": {
            "user_named": user_named,
            "still_current": still_current,
            "too_short": too_short,
            "unknown_provenance": unknown,
        },
    })))
}

/// POST /api/v1/sessions/{session_id}/retitle — rename this one conversation.
/// Overrides even a hand-typed name (a click is consent); refuses only too-short chats.
async fn retitle_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    use pond_core::shared::services::session_title::{RetitleOutcome, SessionTitleService};

    state
        .session_storage
        .get_session(&session_id)
        .await
        .map_err(|e| {
            let status = match &e {
                SessionStorageError::SessionNotFound(_) => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({ "error": format!("{e}") })))
        })?;

    let Some(provider) = state.llm_provider.read().await.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "No language model is configured" })),
        ));
    };

    let service = SessionTitleService::new(provider, state.session_storage.clone());
    let cancel = tokio_util::sync::CancellationToken::new();

    let outcome = service
        .retitle_now(&session_id, &cancel)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("{e}") })),
            )
        })?;

    let body = match outcome {
        RetitleOutcome::Retitled { title, .. } => {
            tracing::info!(
                target: "giap::trace",
                kind = "session_retitled",
                trigger = "manual_one",
                session_id = %session_id,
                title = %title,
            );
            json!({ "session_id": session_id, "outcome": "retitled", "title": title })
        }
        // A declining button must say so, or it looks broken.
        RetitleOutcome::Skipped(reason) => {
            json!({ "session_id": session_id, "outcome": "skipped", "reason": reason.as_str(), "title": Value::Null })
        }
        RetitleOutcome::Unusable => {
            json!({ "session_id": session_id, "outcome": "unusable", "title": Value::Null })
        }
        RetitleOutcome::Cancelled => {
            json!({ "session_id": session_id, "outcome": "cancelled", "title": Value::Null })
        }
    };

    Ok(Json(body))
}

/// Percent-encodes a path segment: `session_id` is caller-supplied on `/chat/stream`.
fn urlencoding_lite(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(ch),
            other => {
                let mut buf = [0u8; 4];
                for b in other.encode_utf8(&mut buf).as_bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    out
}

/// Serves one attachment's raw bytes; `session_id` must own it, so ids can't cross sessions.
async fn get_session_attachment(
    State(state): State<Arc<AppState>>,
    Path((session_id, attachment_id)): Path<(String, String)>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    use axum::response::IntoResponse;

    let owns_it = state
        .session_storage
        .list_session_attachments(&session_id)
        .await
        .map(|atts| atts.iter().any(|a| a.id == attachment_id))
        .unwrap_or(false);
    if !owns_it {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Attachment not found"})),
        ));
    }

    match state.session_storage.read_attachment(&attachment_id).await {
        Ok(Some((mime_type, bytes))) => {
            let headers = [
                (axum::http::header::CONTENT_TYPE, mime_type),
                // Random id, never rewritten: safe to cache hard.
                (
                    axum::http::header::CACHE_CONTROL,
                    "private, max-age=31536000, immutable".to_string(),
                ),
            ];
            Ok((headers, bytes).into_response())
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Attachment bytes are no longer available"})),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{}", e)})),
        )),
    }
}

/// LAN address for phones that can't resolve mDNS (Android). Probes the route to the mDNS
/// group, not the internet, which may leave via a VPN; UDP `connect` sends nothing.
fn lan_address() -> Option<String> {
    use std::net::UdpSocket;

    // RFC1918 gateways back up hosts with an unusual multicast route.
    for probe in [
        "224.0.0.251:5353",
        "192.168.0.1:80",
        "10.0.0.1:80",
        "172.16.0.1:80",
    ] {
        let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
            continue;
        };
        if socket.connect(probe).is_err() {
            continue;
        }
        let Ok(addr) = socket.local_addr() else {
            continue;
        };
        let std::net::IpAddr::V4(v4) = addr.ip() else {
            continue;
        };
        // Loopback and link-local (169.254/16, a failed DHCP) reach nobody.
        if v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() {
            continue;
        }
        return Some(v4.to_string());
    }
    None
}

/// True for Tailscale's CGNAT range, 100.64.0.0/10. Needed because the probe below also
/// succeeds without a tailnet, returning the LAN address.
fn is_tailnet_v4(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (64..=127).contains(&b)
}

/// This Pond's Tailscale address, when on a tailnet: probes the route to 100.100.100.100
/// (MagicDNS). Clients prefer `lan_address` at home and fall back to this.
fn tailnet_address() -> Option<String> {
    use std::net::UdpSocket;

    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("100.100.100.100:53").ok()?;
    let std::net::IpAddr::V4(v4) = socket.local_addr().ok()?.ip() else {
        return None;
    };

    is_tailnet_v4(v4).then(|| v4.to_string())
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
        // Null without a LAN route; for clients that can't resolve `<hostname>.local`.
        "lan_address": lan_address(),
        // Null off a tailnet; the phone's fallback when the LAN address doesn't answer.
        "tailnet_address": tailnet_address(),
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

/// `GET /api/v1/matter/status`; polled by the Devices tab while a controller installs/starts.
async fn matter_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let status = match &state.matter {
        Some(matter) => matter.status().await,
        // No Matter support wired: plain "off", as there is nothing for the user to fix.
        None => pond_core::user_data::ports::matter_runtime::MatterStatus::disabled(),
    };
    Json(serde_json::to_value(status).unwrap_or_else(|_| json!({})))
}

/// The live commissioner, or an error naming what's actually wrong (off vs unreachable).
async fn matter_commissioner(
    state: &Arc<AppState>,
) -> Result<
    Arc<dyn pond_core::user_data::ports::device_commissioning::DeviceCommissioningPort>,
    (StatusCode, Json<Value>),
> {
    use pond_core::user_data::ports::matter_runtime::{MatterState, MatterStatus};

    let status = match &state.matter {
        Some(matter) => {
            if let Some(commissioner) = matter.commissioner().await {
                return Ok(commissioner);
            }
            matter.status().await
        }
        None => MatterStatus::disabled(),
    };

    Err(match status.state {
        // Only reachable on a build without the Matter feature, so name the build, not a switch.
        MatterState::Disabled => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "This build of GIAP has no Matter support compiled in, so there \
                          is nothing to turn on. Reinstall with the default features to \
                          use Matter devices."
            })),
        ),
        MatterState::Connecting => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "Matter is still starting up — the controller is not ready yet. \
                          Try again in a moment."
            })),
        ),
        MatterState::Unreachable { error } => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": format!(
                    "Matter is on, but the controller at {} could not be reached: {error}",
                    status.url
                )
            })),
        ),
        // Torn down between the two reads: transient.
        MatterState::Connected => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "The Matter controller connection just dropped. Try again in a moment."
            })),
        ),
    })
}

/// `POST /api/v1/devices/commission` — bring a Matter device onto the fabric. The optional
/// `name` becomes its NodeLabel and registry name, so chat can resolve it.
async fn commission_device(
    State(state): State<Arc<AppState>>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;

    let commissioner = matter_commissioner(&state).await?;

    let raw = req.get("code").and_then(Value::as_str).unwrap_or_default();
    let code =
        pond_core::user_data::ports::device_commissioning::parse_setup_code(raw).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
        })?;

    let name = req
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let device = commissioner
        .commission(code, name.clone())
        .await
        // `{e:#}`: `to_string()` prints only the outer context and drops the controller's error.
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("{e:#}")})),
            )
        })?;

    // Write the row here: the bridge's async discovery would race the chosen name.
    let registry = &state.device_registry;
    let exists = registry
        .get_device(&device.device_id)
        .await
        .map(|d| d.is_some())
        .unwrap_or(false);
    if exists {
        if name.is_some() {
            if let Err(e) = registry.rename(&device.device_id, &device.name).await {
                tracing::warn!(device = %device.device_id, error = %e, "commission: rename failed");
            }
        }
    } else if let Err(e) = registry
        .register(
            pond_core::user_data::ports::device_registry::RegisterDeviceRequest {
                id: Some(device.device_id.clone()),
                name: device.name.clone(),
                device_type: device.device_type.clone(),
                hostname: None,
                capabilities: device.capabilities.clone(),
                room: None,
            },
        )
        .await
    {
        tracing::warn!(device = %device.device_id, error = %e, "commission: registry write failed");
    }

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id":      device.device_id,
            "name":    device.name,
            "node_id": device.node_id,
        })),
    ))
}

/// Ids of the devices behind a Matter hub. The trailing `-` stops `matter-9` matching `matter-90`.
fn bridged_children_of(
    hub_id: &str,
    devices: &[pond_core::user_data::ports::device_registry::Device],
) -> Vec<String> {
    let prefix = format!("{hub_id}-");
    devices
        .iter()
        .filter(|d| d.id.starts_with(&prefix))
        .map(|d| d.id.clone())
        .collect()
}

async fn unregister_device(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    use pond_core::user_data::ports::device_commissioning::{
        matter_bridged_endpoint, matter_node_id,
    };

    // Refuse bridged endpoints: Matter decommissions whole nodes (hub + siblings), and dropping
    // just the row lets the controller re-announce it; the hub's own app owns its children.
    if matter_bridged_endpoint(&id).is_some() {
        let hub_id = matter_node_id(&id)
            .map(|node| format!("matter-{node}"))
            .unwrap_or_default();
        let hub_name = state
            .device_registry
            .get_device(&hub_id)
            .await
            .ok()
            .flatten()
            .map(|d| d.name)
            .unwrap_or_else(|| hub_id.clone());
        let siblings = state
            .device_registry
            .list_devices()
            .await
            .map(|devices| bridged_children_of(&hub_id, &devices).len())
            .unwrap_or(0);

        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!(
                    "This device is provided by '{hub_name}'. Remove it in that hub's own app, \
                     or delete '{hub_name}' to remove all {siblings} devices behind it."
                ),
                "hub_id": hub_id,
            })),
        ));
    }

    // Leave the fabric first or the controller re-announces the device; refuse if unreachable.
    if let Some(node_id) = matter_node_id(&id) {
        let commissioner = matter_commissioner(&state).await?;
        commissioner.decommission(node_id).await.map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": format!("could not remove the device from the fabric: {e}")
                })),
            )
        })?;
    }

    // A hub's children leave the fabric with it, so drop their rows too (they'd be undeletable).
    // Children first: a part-way failure leaves the hub visible and re-deletable.
    let children = match state.device_registry.list_devices().await {
        Ok(devices) => bridged_children_of(&id, &devices),
        Err(e) => {
            tracing::warn!(device = %id, error = %e, "devices: could not list to cascade a hub delete");
            Vec::new()
        }
    };
    for child in &children {
        if let Err(e) = state.device_registry.unregister(child).await {
            tracing::warn!(device = %child, error = %e, "devices: could not remove a bridged child");
        }
    }
    if !children.is_empty() {
        tracing::info!(
            target: "giap::trace",
            kind = "devices_hub_removed",
            device = %id,
            children = children.len(),
            "devices: removed a hub and the devices behind it"
        );
    }

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

/// `POST /api/v1/devices/{id}/offline` — manual "Turn off" for devices with no liveness signal.
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

// ── Private mesh ─────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct AddMeshPeerRequest {
    peer_id: String,
    trust_scope: String,
    /// Best-effort dial hint used once after trusting; `PeerDirectory` doesn't persist it.
    #[serde(default)]
    address: Option<String>,
}

#[derive(serde::Deserialize)]
struct CreditMeshPeerRequest {
    amount_millisats: u64,
}

fn trust_scope_from_str(s: &str) -> Result<TrustScope, (StatusCode, Json<Value>)> {
    match s {
        "self_owned" => Ok(TrustScope::SelfOwned),
        "circle" => Ok(TrustScope::Circle),
        other => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("unknown trust_scope: '{other}'")})),
        )),
    }
}

fn trust_scope_to_str(scope: TrustScope) -> &'static str {
    match scope {
        TrustScope::SelfOwned => "self_owned",
        TrustScope::Circle => "circle",
    }
}

fn parse_mesh_peer_id(s: &str) -> Result<MeshPeerId, (StatusCode, Json<Value>)> {
    s.parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid peer_id"})),
        )
    })
}

fn mesh_internal_error(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": e.to_string()})),
    )
}

/// `GET /api/v1/mesh/peers` — trusted peers with live connection status and credit balance.
async fn list_mesh_peers(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let peers = state
        .peer_directory
        .list_trusted_peers(None)
        .await
        .map_err(mesh_internal_error)?;

    let transport = state.mesh_transport.read().await.clone();
    let connected: std::collections::HashSet<MeshPeerId> = match &transport {
        Some(transport) => transport
            .connected_peers()
            .await
            .unwrap_or_default()
            .into_iter()
            .collect(),
        None => std::collections::HashSet::new(),
    };

    let mut list = Vec::with_capacity(peers.len());
    for peer in peers {
        let scope = state
            .peer_directory
            .trust_scope_of(peer)
            .await
            .map_err(mesh_internal_error)?
            .unwrap_or(TrustScope::Circle);
        let balance = state
            .credit_ledger
            .balance(peer)
            .await
            .map_err(mesh_internal_error)?;
        list.push(json!({
            "peer_id": peer.to_string(),
            "trust_scope": trust_scope_to_str(scope),
            "connected": connected.contains(&peer),
            "credit_balance_millisats": balance.value(),
        }));
    }

    Ok(Json(json!({ "peers": list })))
}

/// `POST /api/v1/mesh/peers` — trust a peer, then best-effort connect; trust stands if that fails.
async fn add_mesh_peer(
    State(state): State<Arc<AppState>>,
    body: Result<Json<AddMeshPeerRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {e}")})),
        )
    })?;
    let peer = parse_mesh_peer_id(&req.peer_id)?;
    let scope = trust_scope_from_str(&req.trust_scope)?;

    state
        .peer_directory
        .add_trusted_peer(peer, scope)
        .await
        .map_err(mesh_internal_error)?;

    let transport = state.mesh_transport.read().await.clone();
    if let (Some(address), Some(transport)) = (&req.address, &transport) {
        if let Err(err) = transport.connect(peer, address.clone()).await {
            tracing::warn!("mesh: connect to newly-trusted peer {peer} failed: {err}");
        }
    }

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "peer_id": peer.to_string(),
            "trust_scope": trust_scope_to_str(scope),
        })),
    ))
}

/// `DELETE /api/v1/mesh/peers/{peer_id}` — revoke trust.
async fn remove_mesh_peer(
    State(state): State<Arc<AppState>>,
    Path(peer_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let peer = parse_mesh_peer_id(&peer_id)?;
    state
        .peer_directory
        .remove_trusted_peer(peer)
        .await
        .map_err(mesh_internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/mesh/peers/{peer_id}/credit` — manual top-up until Lightning settlement exists.
/// Checks trust itself: `CreditLedger` would fund any `PeerId`, trusted or not.
async fn credit_mesh_peer(
    State(state): State<Arc<AppState>>,
    Path(peer_id): Path<String>,
    body: Result<Json<CreditMeshPeerRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {e}")})),
        )
    })?;
    if req.amount_millisats == 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "amount_millisats must be greater than zero"})),
        ));
    }
    let peer = parse_mesh_peer_id(&peer_id)?;

    let scope = state
        .peer_directory
        .trust_scope_of(peer)
        .await
        .map_err(mesh_internal_error)?;
    if scope.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "peer is not trusted — add them first"})),
        ));
    }

    state
        .credit_ledger
        .credit(peer, Millisats::new(req.amount_millisats))
        .await
        .map_err(mesh_internal_error)?;
    let balance = state
        .credit_ledger
        .balance(peer)
        .await
        .map_err(mesh_internal_error)?;

    Ok(Json(json!({
        "peer_id": peer.to_string(),
        "credit_balance_millisats": balance.value(),
    })))
}

/// `GET /api/v1/mesh/peers/{peer_id}/capabilities` — queried live over the mesh, not cached.
async fn get_mesh_peer_capabilities(
    State(state): State<Arc<AppState>>,
    Path(peer_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let peer = parse_mesh_peer_id(&peer_id)?;

    let scope = state
        .peer_directory
        .trust_scope_of(peer)
        .await
        .map_err(mesh_internal_error)?;
    if scope.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "peer is not trusted — add them first"})),
        ));
    }

    let query = state.peer_capability_query.read().await.clone();
    let Some(query) = query else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "mesh is not enabled on this Pond"})),
        ));
    };
    let capabilities = query
        .capabilities_of(peer)
        .await
        .map_err(mesh_internal_error)?;

    Ok(Json(json!({
        "peer_id": peer.to_string(),
        "inference_available": capabilities.inference_available,
        "lightning_available": capabilities.lightning_available,
    })))
}

/// `GET /api/v1/mesh/settlement` — the fixed rate and each peer's pending usage (read-only).
async fn get_mesh_settlement_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let rate = pond_core::mesh::domain::settlement::MESH_SETTLEMENT_MILLISATS_PER_TOKEN;

    let peers = state
        .peer_directory
        .list_trusted_peers(None)
        .await
        .map_err(mesh_internal_error)?;

    let mut peer_list = Vec::with_capacity(peers.len());
    for peer in peers {
        // Borrowed: we owe, settlement pays. Lent: they owe us, display only (theirs to collect).
        let borrowed = state
            .usage_tally
            .pending_borrowed(peer)
            .await
            .map_err(mesh_internal_error)?;
        let lent = state
            .usage_tally
            .pending_lent(peer)
            .await
            .map_err(mesh_internal_error)?;
        peer_list.push(json!({
            "peer_id": peer.to_string(),
            "pending_tokens": borrowed.value(),
            "pending_millisats": borrowed.value().saturating_mul(rate),
            "owed_to_us_tokens": lent.value(),
            "owed_to_us_millisats": lent.value().saturating_mul(rate),
        }));
    }

    Ok(Json(json!({
        "configured": rate > 0,
        "millisats_per_token": rate,
        "peers": peer_list,
    })))
}

/// `GET /api/v1/mesh/self` — peer id + invite link; mesh off returns 200 so the UI can prompt.
async fn get_mesh_self(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let transport = state.mesh_transport.read().await.clone();
    let Some(transport) = transport else {
        return Ok(Json(json!({ "mesh_enabled": false })));
    };
    let peer_id = transport.local_peer_id();
    let addresses = transport.listen_addresses().await.unwrap_or_default();
    // Prefer a non-loopback address: an invite is for a peer on another machine.
    let address = addresses
        .iter()
        .find(|a| !a.contains("127.0.0.1") && !a.contains("/ip6/::1/"))
        .or_else(|| addresses.first())
        .cloned();
    let invite_url = match &address {
        Some(address) => format!(
            "pond-mesh://invite?peer={peer_id}&addr={}",
            urlencoding::encode(address)
        ),
        None => format!("pond-mesh://invite?peer={peer_id}"),
    };
    Ok(Json(json!({
        "mesh_enabled": true,
        "peer_id": peer_id.to_string(),
        "invite_url": invite_url,
    })))
}

#[derive(serde::Deserialize)]
struct RegisterPushTokenRequest {
    token: String,
    /// "fcm" | "apns" | "expo".
    platform: String,
}

/// Max stored push-token length; real FCM/APNs/Expo tokens are well under 1 KB.
const MAX_PUSH_TOKEN_LEN: usize = 4096;

/// `POST /api/v1/devices/{id}/push-token` — set a device's push token, replacing any prior one.
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
    // Real tokens are printable ASCII; this keeps control chars out of logs and the relays.
    if !req.token.chars().all(|c| c.is_ascii_graphic()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "`token` must be printable ASCII" })),
        ));
    }

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

/// `DELETE /api/v1/devices/{id}/push-token` — idempotent; used on logout/unpair.
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

/// The trimmed name to geocode on save, if any. A coordinate edit is a value change, not key
/// presence: the Settings page echoes every key on each save.
#[allow(clippy::too_many_arguments)]
fn geocode_target(
    merged_name: &str,
    current_name: &str,
    merged_lat: f64,
    merged_lon: f64,
    patch_lat: Option<f64>,
    patch_lon: Option<f64>,
    current_lat: f64,
    current_lon: f64,
) -> Option<String> {
    let name = merged_name.trim();
    if name.is_empty() {
        return None;
    }
    let coords_edited =
        patch_lat.is_some_and(|v| v != current_lat) || patch_lon.is_some_and(|v| v != current_lon);
    if coords_edited {
        return None;
    }
    let name_changed = merged_name != current_name;
    let coords_unset = merged_lat == 0.0 && merged_lon == 0.0;
    (name_changed || coords_unset).then(|| name.to_string())
}

async fn update_settings(
    State(state): State<Arc<AppState>>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(mut patch) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid settings body: {}", e)})),
        )
    })?;

    let current = state.settings_repo.get().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to load current settings: {}", e)})),
        )
    })?;

    // Canonicalises in place (`9:05` -> `09:05`) and reports every failing field, not the first.
    if let Err(errors) =
        pond_core::user_data::domain::settings_validation::validate_patch(&mut patch)
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": pond_core::user_data::domain::settings_validation::render_errors(&errors),
                // Per field too, so a form can mark the offending box.
                "fields": errors
                    .iter()
                    .map(|e| json!({ "field": e.field, "message": e.message }))
                    .collect::<Vec<_>>(),
            })),
        ));
    }

    if patch.get("agent_backend").and_then(|v| v.as_str()) == Some("pond") {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "agent_backend \"pond\" is not available — backend is not yet production-ready. Use \"goose\"."
            })),
        ));
    }

    let mut base =
        serde_json::to_value(&current).unwrap_or(serde_json::Value::Object(Default::default()));
    if let (Some(base_obj), Some(patch_obj)) = (base.as_object_mut(), patch.as_object()) {
        for (k, v) in patch_obj {
            base_obj.insert(k.clone(), v.clone());
        }
    }
    // Fail loudly: falling back to `current` would return 200 while discarding the edits.
    let mut merged: Settings = serde_json::from_value(base).map_err(|e| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": format!("Invalid settings value: {}", e)})),
        )
    })?;

    // Validate the URL at save time, but only when Matter is edited, so a bad stored value can't
    // block unrelated saves. `matter_ble_enabled` counts: the controller must restart for it.
    let touches_matter = patch
        .as_object()
        .is_some_and(|o| o.contains_key("matter_ws_url") || o.contains_key("matter_ble_enabled"));
    let matter_url = merged.matter_ws_url.trim();
    if touches_matter && !(matter_url.starts_with("ws://") || matter_url.starts_with("wss://")) {
        // Say "empty" explicitly: the field's placeholder hides that it is blank.
        let message = if matter_url.is_empty() {
            "Controller address is empty. Enter the Matter controller's WebSocket URL, \
             for example ws://127.0.0.1:5580/giap"
        } else {
            "Controller address must be a WebSocket URL, for example ws://127.0.0.1:5580/giap"
        };
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": message, "field": "matter_ws_url" })),
        ));
    }

    // Write only the patch's keys, or a concurrent writer's changes get reverted. `user_keys` is
    // the user's intent (fed to `mark_user_set`); `write_keys` adds server-derived values.
    let user_keys: std::collections::HashSet<String> = patch
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    let mut write_keys = user_keys.clone();

    // Geocode best-effort: on failure the weather adapter resolves the name on demand anyway.
    let patch_lat = patch.get("weather_latitude").and_then(|v| v.as_f64());
    let patch_lon = patch.get("weather_longitude").and_then(|v| v.as_f64());
    if let Some(name) = geocode_target(
        &merged.weather_location_name,
        &current.weather_location_name,
        merged.weather_latitude,
        merged.weather_longitude,
        patch_lat,
        patch_lon,
        current.weather_latitude,
        current.weather_longitude,
    ) {
        let geocoder = pond_adapters_weather::Geocoder::new(state.http_client.clone());
        match geocoder.geocode(&name).await {
            Ok(geo) => {
                merged.weather_latitude = geo.latitude;
                merged.weather_longitude = geo.longitude;
                write_keys.insert("weather_latitude".to_string());
                write_keys.insert("weather_longitude".to_string());
            }
            Err(e) => tracing::warn!(
                error = %e,
                location = %name,
                "geocode-on-save failed; keeping provided coordinates"
            ),
        }
    }

    state
        .settings_repo
        .update_fields(&merged, Some(&write_keys))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to save settings: {}", e)})),
            )
        })?;

    // Must follow the write (marks existing rows); exempts these keys from DEFAULT_ADOPTIONS.
    if let Err(e) = state.settings_repo.mark_user_set(&user_keys).await {
        tracing::warn!(error = %e, "failed to record user intent for settings patch");
    }

    // Privacy controls must apply now, not at the next restart.
    pond_core::models::domain::mic_gate::set_mic_enabled(merged.mic_enabled);

    // Unconditional: it's cheap, and a skipped re-install would silently drop the restriction.
    pond_core::shared::services::egress::set_network_mode(
        pond_core::shared::services::egress::NetworkMode::parse(&merged.network_mode),
    );

    // Only on a Matter edit: the reconciler restarts a not-yet-Connected controller, so any save
    // would restart a multi-minute first install. The Devices tab's Retry sends `matter_ws_url`.
    if touches_matter {
        if let Some(matter) = &state.matter {
            matter.apply(pond_core::user_data::ports::matter_runtime::MatterConfig {
                url: merged.matter_ws_url.trim().to_string(),
                ble: merged.matter_ble_enabled,
            });
        }
    }

    // `mesh_rebuild` is always safe (no-ops if built); gated only to spare other saves a read.
    let touches_mesh = patch
        .as_object()
        .is_some_and(|o| o.contains_key("mesh_enabled"));
    if touches_mesh && merged.mesh_enabled {
        if let Some(rebuild) = &state.mesh_rebuild {
            rebuild().await;
            // If chat_provider was already "mesh", swap out the cached UnavailableProvider now.
            rebuild_llm_provider(&state, &merged).await;
        }
    }

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

            // Mirror to model_role_assignments, the source of truth the CLI and `/activate` see.
            if let Some(repo) = &state.model_repo {
                let role_map: &[(&str, &str, &str)] = &[
                    ("chat", &merged.chat_provider, &merged.chat_model),
                    ("asr", "", &merged.active_whisper_model),
                    ("tts", "", &merged.active_tts_model),
                ];
                for (role, provider, model_name) in role_map {
                    // No catalog row to assign. "mesh" is a peer's compute and would hit
                    // `for_chat_provider`'s "llamafile" catch-all.
                    if model_name.is_empty() || (*role == "chat" && *provider == "mesh") {
                        let _ = repo.clear_assignment(role).await;
                        continue;
                    }
                    // Same mapping as the context governor's lookup, so both address one row.
                    let category = match *role {
                        "asr" => "whisper",
                        "tts" => "tts_piper",
                        _ => ModelCategory::for_chat_provider(provider).as_str(),
                    };
                    let model_id = format!("{}/{}", category, model_name);
                    let _ = repo.set_assignment(role, &model_id).await;
                }
            }
        }
    }

    // The warmed prefix is now stale; re-warm in the background so the save doesn't wait.
    if current.chat_provider != merged.chat_provider || current.chat_model != merged.chat_model {
        crate::spawn_prefix_prewarm(state.clone(), false);
    }

    // f32 fields echo widened (`0.7` -> 0.699999988079071); clients must diff against the patch
    // they sent. Don't round here: that would report a value the server doesn't hold.
    Ok(Json(
        serde_json::to_value(&merged).unwrap_or(json!({ "status": "ok" })),
    ))
}

/// Prefix warm-up status for the boot banner and voice greeting gate.
async fn get_warmup(State(state): State<Arc<AppState>>) -> Json<Value> {
    let snapshot = state
        .warmup
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    let elapsed_ms = match (snapshot.started_unix_ms, snapshot.finished_unix_ms) {
        (0, _) => 0,
        (s, Some(f)) => f.saturating_sub(s),
        (s, None) => now.saturating_sub(s),
    };
    let mut v = serde_json::to_value(&snapshot).unwrap_or_else(|_| json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("elapsed_ms".into(), json!(elapsed_ms));
    }
    Json(v)
}

/// Weather widget data; `{"enabled": false}` (not an error) when no location is set.
async fn get_weather(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(provider) = state.weather_provider.as_ref() else {
        return Ok(Json(json!({ "enabled": false })));
    };

    // `{e:#}` keeps the whole anyhow chain, where an egress refusal's reason, mode and host live.
    let current = provider.current().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("Failed to fetch weather: {e:#}")})),
        )
    })?;
    let forecast = provider.forecast(4).await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("Failed to fetch forecast: {e:#}")})),
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

/// Maps a WMO description to one of the dashboard widget's icon keys.
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

fn weather_short_weekday(date: &str) -> String {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(|d| d.format("%a").to_string())
        .unwrap_or_else(|_| date.to_string())
}

use pond_core::models::ports::provider::UnavailableProvider;

async fn rebuild_llm_provider(state: &Arc<AppState>, settings: &Settings) {
    use pond_adapters_llamafile::LlamafileProvider;
    use pond_adapters_ollama::OllamaProvider;
    #[allow(unused_imports)]
    use pond_core::models::ports::provider::LlmProvider as _;

    let url = &state.llamafile_url;
    let data_dir = state.data_dir.clone();
    let max_tokens = settings.llm_max_tokens;
    let temperature = settings.llm_temperature;

    /// Builds the provider for one (provider, model) pair. Never constructs a mesh provider:
    /// the transport has a single `recv()` consumer, so only `mesh_rebuild` may create one.
    async fn build_one(
        provider: &str,
        model: &str,
        url: &str,
        _data_dir: Option<std::path::PathBuf>,
        max_tokens: u32,
        temperature: f32,
        mesh_provider: Option<Arc<dyn LlmProvider>>,
        _model_repo: Option<
            Arc<dyn pond_core::models::ports::model_repository::ModelRepository + Send + Sync>,
        >,
    ) -> Arc<dyn LlmProvider> {
        match provider {
            "mesh" => mesh_provider.unwrap_or_else(|| {
                Arc::new(UnavailableProvider::new(
                    "mesh inference is not enabled on this Pond (mesh_enabled is off, \
                     or this build lacks the `mesh` feature)",
                )) as Arc<dyn LlmProvider>
            }),

            "ollama" => Arc::new(
                OllamaProvider::new(None, Some(model))
                    .with_max_tokens(max_tokens)
                    .with_temperature(temperature),
            ) as Arc<dyn LlmProvider>,

            #[cfg(feature = "local-inference")]
            "local" | "gguf" => {
                use pond_adapters_local_inference::LocalInferenceLlmAdapter;

                // Look up the filename: it needn't be `{model}.gguf` for the catalog alias.
                let resolved_filename = match &_model_repo {
                    Some(repo) => repo
                        .get_by_id(&format!("gguf/{model}"))
                        .await
                        .ok()
                        .flatten()
                        .and_then(|record| record.filename),
                    None => None,
                };
                let model_arg = resolved_filename.as_deref().unwrap_or(model);

                let result = match &_data_dir {
                    Some(dir) => LocalInferenceLlmAdapter::new_with_data_dir(model_arg, dir).await,
                    None => LocalInferenceLlmAdapter::new(model_arg).await,
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
        state.mesh_provider.read().await.clone(),
        state.model_repo.clone(),
    )
    .await;
    // Start llamafile before the swap so the first request doesn't time out.
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

/// GET /api/v1/models/active-roles — per-role model, from `model_role_assignments` + settings.
async fn get_active_roles(State(state): State<Arc<AppState>>) -> Json<Value> {
    let assignments: std::collections::HashMap<String, String> = state
        .model_repo
        .as_ref()
        .and_then(|r| {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(r.list_assignments())
            })
            .ok()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|a| (a.role, a.model_id))
        .collect();

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

/// Finds a model, retrying the other TTS engines for a TTS category (never for others).
/// The models list groups every TTS engine as `tts`, which parses as `tts_piper`.
async fn find_model_forgiving_tts(
    repo: &dyn pond_core::models::ports::model_repository::ModelRepository,
    cat: &ModelCategory,
    name: &str,
) -> Result<Option<ModelRecord>, anyhow::Error> {
    if let Some(m) = repo.get_by_id(&ModelRecord::id_for(cat, name)).await? {
        return Ok(Some(m));
    }
    if !cat.is_tts() {
        return Ok(None);
    }
    for alt in [
        ModelCategory::TtsKokoro,
        ModelCategory::TtsPiper,
        ModelCategory::TtsHttp,
    ] {
        if &alt == cat {
            continue;
        }
        if let Some(m) = repo.get_by_id(&ModelRecord::id_for(&alt, name)).await? {
            return Ok(Some(m));
        }
    }
    Ok(None)
}

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
        context_length: m.context_length,
        asr_language: m.asr_language.clone(),
        asr_size: m.asr_size.clone(),
        tts_engine: m.tts_engine.clone(),
        config_filename: m.config_filename.clone(),
    }
}

/// Parses a GGUF header from the first MiB, which holds every useful key; `None` if not GGUF.
fn read_gguf_head(path: &std::path::Path) -> Option<pond_core::models::domain::gguf::GgufInfo> {
    use std::io::Read as _;
    const HEAD_BYTES: usize = 1024 * 1024;

    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; HEAD_BYTES];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    pond_core::models::domain::gguf::parse_gguf_header(&buf)
}

/// Parses `(language, size)` from whisper.cpp's file naming; a ggml `.bin` has no header.
fn whisper_facts_from_name(name: &str) -> (Option<String>, Option<String>) {
    let lower = name.to_lowercase();
    let size = ["large", "medium", "small", "base", "tiny"]
        .iter()
        .find(|s| lower.contains(*s))
        .map(|s| (*s).to_string());
    // Only when the size is recognised, so an unrelated .bin isn't labelled whisper.
    let language = size
        .as_ref()
        .map(|_| {
            if lower.contains(".en") || lower.ends_with("-en") {
                "en"
            } else {
                "multilingual"
            }
        })
        .map(str::to_string);
    (language, size)
}

/// Adds model files found on disk but missing from the catalog as custom entries; returns them.
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
                    // Follows symlinks, unlike `entry.metadata()`: HF cache models are symlinks.
                    let path = entry.path();
                    let size_mb = std::fs::metadata(&path)
                        .map(|m| m.len() / 1_048_576)
                        .unwrap_or(0);

                    let gguf = if fname.ends_with(".gguf") {
                        read_gguf_head(&path)
                    } else {
                        None
                    };

                    let name = fname
                        .trim_end_matches(".gguf")
                        .trim_end_matches(".llamafile")
                        .trim_end_matches(".onnx")
                        .trim_end_matches(".bin")
                        .to_string();

                    let whisper = if matches!(category, ModelCategory::Whisper) {
                        whisper_facts_from_name(&name)
                    } else {
                        (None, None)
                    };

                    // The client shows `description` as the name, so no specs here; it swaps
                    // the placeholder for the filename.
                    let description = gguf
                        .as_ref()
                        .and_then(|g| g.name.clone())
                        .filter(|n| !n.trim().is_empty())
                        .unwrap_or_else(|| "(detected on disk)".to_string());

                    found.push(ModelRecord {
                        id: ModelRecord::id_for(&category, &name),
                        category: category.clone(),
                        name,
                        filename: Some(fname),
                        description,
                        size_mb,
                        url: None,
                        hf_id: None,
                        // Weights + 25% for the run, the catalogue's own rule of thumb.
                        ram_estimate_mb: (size_mb > 0).then(|| size_mb + size_mb / 4),
                        recommended_role: None,
                        context_length: gguf.as_ref().and_then(|g| g.context_length),
                        quantization: gguf.as_ref().and_then(|g| g.quantization.clone()),
                        asr_language: whisper.0,
                        asr_size: whisper.1,
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
        // Hand-copied voices: the catalogue lists only the English ones of the repo's 50-odd.
        extras.extend(scan_dir(
            data_dir_owned.join("models").join("kokoro").join("voices"),
            ModelCategory::TtsKokoro,
            &[".bin"],
        ));
        extras
    })
    .await
    .unwrap_or_default();

    for m in &extras_from_disk {
        let _ = model_repo.upsert(m).await;
    }

    extras_from_disk
}

/// Where a downloaded model lands, by category; shared so a resume finds its partial file.
fn model_dest_path(
    data_dir: &std::path::Path,
    category: &str,
    filename: &str,
) -> std::path::PathBuf {
    match category {
        "whisper" => data_dir.join("models").join(filename),
        "llamafile" => data_dir.join("models").join("llm").join(filename),
        "gguf" => data_dir.join("models").join("gguf").join(filename),
        "tts" | "tts_piper" => data_dir.join("models").join("tts").join(filename),
        "tts_kokoro" => data_dir
            .join("models")
            .join("kokoro")
            .join("voices")
            .join(filename),
        _ => data_dir.join("models").join(filename),
    }
}

/// `POST /api/v1/models/download/control` — pause, resume or cancel. Filename is in the body
/// (it has dots and slashes). Pause keeps the partial file for a resume; cancel deletes it.
async fn download_control(
    State(state): State<Arc<AppState>>,
    body: Result<Json<Value>, JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let Ok(Json(body)) = body else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid request body"})),
        );
    };

    let filename = body["filename"].as_str().unwrap_or_default().to_string();
    let action = body["action"].as_str().unwrap_or_default().to_string();
    if filename.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "filename is required"})),
        );
    }

    let (category, url) = {
        let t = state.download_tracker.read().await;
        let Some(entry) = t.get(&filename) else {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("no download named {filename}")})),
            );
        };
        match action.as_str() {
            "pause" => {
                entry
                    .control
                    .store(crate::DL_PAUSE, std::sync::atomic::Ordering::Relaxed);
                return (StatusCode::OK, Json(json!({"status": "pausing"})));
            }
            "cancel" => {
                entry
                    .control
                    .store(crate::DL_CANCEL, std::sync::atomic::Ordering::Relaxed);
                return (StatusCode::OK, Json(json!({"status": "cancelling"})));
            }
            "resume" => (entry.category.clone(), entry.url.clone()),
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "action must be pause, resume or cancel"})),
                )
            }
        }
    };

    // A fresh transfer resumes: the HF cache finds its `.incomplete` and sends a Range header.
    let Some(url) = url else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "this download cannot be resumed — its source was not recorded"})),
        );
    };
    let Some(data_dir) = state.data_dir.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "data_dir not configured"})),
        );
    };

    let dest = model_dest_path(&data_dir, &category, &filename);
    let tracker = Arc::clone(&state.download_tracker);
    let client = state.http_client.clone();

    tokio::spawn(async move {
        spawn_tracked_download(
            url,
            dest,
            filename,
            category,
            tracker,
            client,
            data_dir,
            async {},
        )
        .await;
    });

    (StatusCode::OK, Json(json!({"status": "resuming"})))
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

    // Scan in the background so the page doesn't wait; hand-added files show on the next load.
    // The flag stops the page's repeated fetches from stacking scans.
    if let Some(data_dir) = &state.data_dir {
        static SCANNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if SCANNING
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
        {
            let dir = data_dir.clone();
            let repo = Arc::clone(model_repo);
            tokio::spawn(async move {
                let _ = scan_filesystem_extras(&dir, &repo).await;
                SCANNING.store(false, std::sync::atomic::Ordering::Release);
            });
        }
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
            ModelCategory::TtsPiper | ModelCategory::TtsKokoro | ModelCategory::TtsHttp => {
                tts.push(v)
            }
            ModelCategory::Gguf => gguf.push(v),
            ModelCategory::Ollama => ollama.push(v),
            ModelCategory::Embedding => embedding.push(v),
        }
    }

    Ok(Json(
        json!({"whisper": whisper, "llamafile": llamafile, "tts": tts, "gguf": gguf, "ollama": ollama, "embedding": embedding}),
    ))
}

/// POST /api/v1/models/scan — synchronous disk scan; persists and returns new entries.
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

/// Registers only models the local Ollama daemon actually has, so `downloaded` is accurate.
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

    tokio::spawn(async move {
        match catalog_provider.fetch().await {
            Ok((models, _binaries)) => {
                let count = models.len();
                for mut m in models {
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
                            pond_core::models::domain::model_record::ModelCategory::TtsKokoro => {
                                data_dir
                                    .join("models")
                                    .join("kokoro")
                                    .join("voices")
                                    .join(f)
                                    .exists()
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

/// GET /api/v1/models/download/progress — also evicts entries finished over 5 min ago.
async fn get_download_progress(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mut tracker = state.download_tracker.write().await;
    let now = std::time::Instant::now();
    tracker.retain(|_, e| match e.finished_at {
        Some(t) => now.duration_since(t) < std::time::Duration::from_secs(300),
        None => true,
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
    let m = find_model_forgiving_tts(model_repo.as_ref(), &cat, &name)
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
    // The found record's id: a forgiving TTS lookup may resolve another category.
    let model_id = m.id.clone();

    if m.downloaded {
        return Ok(Json(json!({"status": "already_downloaded", "name": name})));
    }

    // fastembed downloads embedding models itself on first use.
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

    // Check egress before spawning so a refusal reaches the caller, not just a failed tracker row.
    pond_core::shared::services::egress::check_egress(&url).map_err(|denied| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": denied.to_string()})),
        )
    })?;

    let dest = match cat {
        ModelCategory::Whisper => data_dir.join("models").join(&filename),
        ModelCategory::Llamafile => data_dir.join("models").join("llm").join(&filename),
        ModelCategory::Gguf => data_dir.join("models").join("gguf").join(&filename),
        ModelCategory::TtsPiper | ModelCategory::TtsHttp => {
            data_dir.join("models").join("tts").join(&filename)
        }
        // Not models/tts/: the Kokoro engine loads voices from its own directory.
        ModelCategory::TtsKokoro => data_dir
            .join("models")
            .join("kokoro")
            .join("voices")
            .join(&filename),
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
                    // A separate hop to its own host: gate it separately from the weights.
                    match pond_core::shared::services::egress::begin(&cu, "GET") {
                        Err(denied) => {
                            tracing::warn!("TTS config file {cf} not fetched: {denied}")
                        }
                        Ok(call) => {
                            let sent = cfg_client.get(&cu).send().await;
                            call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
                            match sent {
                                Ok(resp) if resp.status().is_success() => {
                                    if let Ok(bytes) = resp.bytes().await {
                                        let _ = tokio::fs::write(&cfg_dest, &bytes).await;
                                    }
                                }
                                _ => tracing::warn!("Failed to download TTS config file {}", cf),
                            }
                        }
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

/// DELETE /api/v1/models/{category}/{name} — deletes the file but keeps the catalog row.
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
    let m = find_model_forgiving_tts(model_repo.as_ref(), &cat, &name)
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
    // The found record's id: a forgiving TTS lookup may resolve another category.
    let model_id = m.id.clone();

    let assignments = model_repo.list_assignments().await.unwrap_or_default();
    if let Some(a) = assignments.iter().find(|a| a.model_id == model_id) {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!("Model is assigned to role '{}'. Deactivate it first.", a.role)
            })),
        ));
    }

    if let (Some(filename), Some(data_dir)) = (&m.filename, &state.data_dir) {
        let path = match cat {
            ModelCategory::Whisper => data_dir.join("models").join(filename),
            ModelCategory::Llamafile => data_dir.join("models").join("llm").join(filename),
            ModelCategory::Gguf => data_dir.join("models").join("gguf").join(filename),
            ModelCategory::TtsPiper | ModelCategory::TtsHttp => {
                data_dir.join("models").join("tts").join(filename)
            }
            // Must match `download_model`'s path, or the file survives and reappears as installed.
            ModelCategory::TtsKokoro => data_dir
                .join("models")
                .join("kokoro")
                .join("voices")
                .join(filename),
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
async fn cleanup_models(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(data_dir) = state.data_dir.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "data_dir not configured"})),
        ));
    };

    // Never delete a blob whose basename matches an assigned model.
    let mut protected: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(model_repo) = &state.model_repo {
        if let Ok(assignments) = model_repo.list_assignments().await {
            for a in &assignments {
                if let Ok(Some(m)) = model_repo.get_by_id(&a.model_id).await {
                    if let Some(fname) = m.filename {
                        protected.insert(fname);
                    }
                    // Legacy migrations left blobs named by bare model name.
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
/// `{total_bytes, by_category, hf_cache_bytes, incomplete_bytes}`, counting each blob once.
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

/// POST /api/v1/models/{category}/{name}/activate — assign to a role; mirrored into settings.
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

    let record = find_model_forgiving_tts(model_repo.as_ref(), &cat, &name)
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
    // The found record's id: a forgiving TTS lookup may resolve another category.
    let model_id = record.id.clone();

    // Runtime provider names, not category names (gguf runs as "local").
    let provider = match cat {
        ModelCategory::Gguf => "local",
        ModelCategory::Llamafile => "llamafile",
        ModelCategory::Ollama => "ollama",
        ModelCategory::Whisper => "asr",
        ModelCategory::TtsPiper => "tts",
        ModelCategory::TtsKokoro => "tts",
        ModelCategory::TtsHttp => "tts",
        ModelCategory::Embedding => "embedding",
    };

    model_repo
        .set_assignment(&role, &model_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;

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
            // `KokoroOutput` reads `voice_tts_voice`. Not for Piper, whose value is a filename.
            // `record.category`, not `cat`: a "tts" request can resolve a `tts_kokoro` record.
            if record.category == ModelCategory::TtsKokoro {
                let _ = settings_repo.set_key("voice_tts_voice", name.clone()).await;
            }
        }
        "embedding" => {
            let _ = settings_repo
                .set_key("active_embedding_model", name.clone())
                .await;
            // Leaves `embedding_provider` alone: the engine is a separate choice from the model.
        }
        _ => {}
    }

    if matches!(role.as_str(), "chat" | "think" | "task") {
        // Log-only; Jetson's fail-closed residency check belongs in the local-inference loader.
        warn_if_model_spills(&state, &record).await;

        let settings = state.settings_repo.get().await.unwrap_or_default();
        rebuild_llm_provider(&state, &settings).await;
    }

    Ok(Json(json!({"role": role, "model_id": model_id})))
}

/// KV cache + system headroom (MB); must match the desktop `modelFit` `DEFAULT_HEADROOM_MB`.
const MEMORY_FIT_HEADROOM_MB: u64 = 1024;

/// Spill verdict, mirroring the desktop `modelFit` helper; `None` means unknown (a 0 input).
fn model_spills_budget(residency_mb: u64, available_for_llm_mb: u64) -> Option<bool> {
    if available_for_llm_mb == 0 || residency_mb == 0 {
        return None;
    }
    let budget = available_for_llm_mb.saturating_sub(MEMORY_FIT_HEADROOM_MB);
    Some(residency_mb > budget)
}

/// Logs, never blocks, when a model won't fit the device's LLM memory budget.
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
             GPU budget. See scripts/jetson/llama-optimization."
        );
    }
}

/// GET /api/v1/models/ollama — proxy Ollama's /api/tags to list available local models.
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
    // Refuse with a 200 + `error`: the dashboard renders that but throws on a non-2xx.
    let call = match pond_core::shared::services::egress::begin(&url, "GET") {
        Ok(c) => c,
        Err(denied) => return Json(json!({"models": [], "error": denied.to_string()})),
    };
    let sent = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .header(
            "user-agent",
            concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await;
    call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
    match sent {
        Ok(resp) if resp.status().is_success() => {
            let models: Vec<Value> = resp.json().await.unwrap_or_default();
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
    // See search_gguf_models for why the refusal rides the body.
    let call = match pond_core::shared::services::egress::begin(url, "GET") {
        Ok(c) => c,
        Err(denied) => return Json(json!({"models": [], "error": denied.to_string()})),
    };
    let sent = client
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .header(
            "user-agent",
            concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await;
    call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
    match sent {
        Ok(resp) if resp.status().is_success() => {
            let releases: Vec<Value> = resp.json().await.unwrap_or_default();
            let mut assets: Vec<Value> = Vec::new();
            for release in &releases {
                let tag = release["tag_name"].as_str().unwrap_or("");
                if let Some(arr) = release["assets"].as_array() {
                    for asset in arr {
                        let name = asset["name"].as_str().unwrap_or("");
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
    // Don't percent-encode: HF returns 400 for `owner%2Fname`.
    let url = format!("https://huggingface.co/api/models/{}", repo);
    let client = &state.http_client;
    // See search_gguf_models for why the refusal rides the body.
    let call = match pond_core::shared::services::egress::begin(&url, "GET") {
        Ok(c) => c,
        Err(denied) => return Json(json!({"files": [], "error": denied.to_string()})),
    };
    let sent = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .header(
            "user-agent",
            concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await;
    call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
    match sent {
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

    // Refuse synchronously, not just in the task: the caller chooses this host.
    if let Err(denied) = pond_core::shared::services::egress::check_egress(&url) {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": denied.to_string()})),
        );
    }

    let Some(data_dir) = state.data_dir.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "data_dir not configured"})),
        );
    };

    let dest = model_dest_path(&data_dir, &category, &filename);

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

/// Tracked download to `dest`; `on_done` runs only on success. HF URLs use `pond_hf_cache`
/// (resumable, keeps auth across HF→CDN redirects).
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
                control: Arc::new(std::sync::atomic::AtomicU8::new(crate::DL_RUN)),
                url: Some(url.clone()),
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
                // Gated here as well as in callers: every non-HF transfer passes this point.
                let call = pond_core::shared::services::egress::begin(&url, "GET")
                    .map_err(|denied| denied.to_string())?;
                let sent = client.get(&url).send().await;
                call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
                let resp = sent.map_err(|e| e.to_string())?;
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

/// Resumable HF fetch into `hf_cache/blobs`; `dest` is symlinked to the blob to keep flat paths.
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

    // The per-chunk callback is sync and can't take the async tracker lock, so share the atomic.
    let control = {
        let t = tracker.read().await;
        t.get(tracker_key)
            .map(|e| Arc::clone(&e.control))
            .unwrap_or_default()
    };

    let tracker_owned = Arc::clone(tracker);
    let tracker_key_owned = tracker_key.to_string();
    let progress_control = Arc::clone(&control);
    let progress = move |downloaded: u64, total: u64| -> bool {
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
        progress_control.load(std::sync::atomic::Ordering::Relaxed) == crate::DL_RUN
    };

    let blob_path = match fetch
        .download_to_blob(&client, token.as_deref(), progress)
        .await
    {
        Ok(p) => p,
        Err(e) if pond_hf_cache::is_stopped(&e) => {
            // Expected. The `.incomplete` file stays for a resume; cancel is this plus a delete.
            let cancelled = control.load(std::sync::atomic::Ordering::Relaxed) == crate::DL_CANCEL;
            let mut t = tracker.write().await;
            if let Some(entry) = t.get_mut(tracker_key) {
                entry.status = if cancelled { "cancelled" } else { "paused" }.to_string();
                entry.finished_at = Some(std::time::Instant::now());
            }
            return Err(if cancelled { "cancelled" } else { "paused" }.to_string());
        }
        Err(e) => return Err(e.to_string()),
    };

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
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let profile = state
        .profile_repo
        .get(&id)
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
                Json(json!({"error": format!("no such profile: {id}")})),
            )
        })?;

    // Count before deleting: most `profile_id` foreign keys cascade.
    let memories = state
        .memory_repo
        .count_for_profile(&id)
        .await
        .unwrap_or_default();
    let faces = match state.face_recognition.as_ref() {
        Some(face) => face
            .list_embeddings(&id)
            .await
            .map(|e| e.len() as u64)
            .unwrap_or_default(),
        None => 0,
    };
    let sessions = state
        .session_storage
        .list_sessions()
        .await
        .map(|all| {
            all.iter()
                .filter(|s| s.profile_id.as_deref() == Some(id.as_str()))
                .count() as u64
        })
        .unwrap_or_default();

    // `primary_profile_id` is a KV row, not a foreign key, so clear it by hand before the delete.
    // Both failures abort: proceeding would leave it dangling.
    let settings = state.settings_repo.get().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("could not read settings, so the member was not deleted: {e}")
            })),
        )
    })?;
    let cleared_primary = if settings.primary_profile_id.as_deref() == Some(id.as_str()) {
        state
            .settings_repo
            .set_key("primary_profile_id", String::new())
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": format!(
                            "could not clear primary_profile_id, so the member was not deleted: {e}"
                        )
                    })),
                )
            })?;
        true
    } else {
        false
    };

    state.profile_repo.delete(&id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;

    Ok(Json(json!({
        "profile_id":   id,
        "display_name": profile.display_name,
        // Sessions are released, not deleted: a conversation isn't solely the speaker's.
        "deleted": {
            "memories":        memories,
            "face_embeddings": faces,
        },
        "released": {
            "sessions": sessions,
        },
        "cleared_primary_profile": cleared_primary,
    })))
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
    // Publish only after the write, so consumers never see an unpersisted reading.
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

/// Caps one history query regardless of `limit`, so a wide window can't load all retention.
const SENSOR_HISTORY_MAX_LIMIT: usize = 1000;

fn sensor_reading_json(r: &SensorReading) -> Value {
    json!({
        "device_id":   r.device_id,
        "sensor_type": r.sensor_type,
        "value":       r.value,
        "unit":        r.unit,
        "recorded_at": r.recorded_at.to_rfc3339(),
    })
}

async fn get_recent_sensors(
    State(state): State<Arc<AppState>>,
    Path(device_id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<SensorQueryParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let agg = params.agg.as_deref().map(str::to_lowercase);
    // Reject rather than serve raw history under an unknown `agg`.
    if let Some(a) = agg.as_deref() {
        if !matches!(a, "min" | "max" | "avg" | "current") {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "invalid `agg`: expected one of min, max, avg, current"
                })),
            ));
        }
    }

    // Parse up front so a malformed bound is an error on every branch, not an unbounded query.
    let since = parse_sensor_time_param("since", params.since.as_deref())?;
    let until = parse_sensor_time_param("until", params.until.as_deref())?;

    let storage_error = |e: anyhow::Error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    };

    // No sensor type: only recent readings across all types make sense.
    let Some(sensor_type) = params.sensor_type.as_deref() else {
        // Refuse `agg`/`since`/`until` here instead of silently ignoring them.
        if agg.is_some() || since.is_some() || until.is_some() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "`agg`, `since` and `until` need a `sensor_type`: \
                              a device can report several, and they cannot be \
                              aggregated together"
                })),
            ));
        }
        let limit = params.limit.unwrap_or(20).min(100);
        let readings = state
            .sensor_storage
            .get_recent(&device_id, limit)
            .await
            .map_err(storage_error)?;
        let list: Vec<Value> = readings.iter().map(sensor_reading_json).collect();
        return Ok(Json(json!({ "readings": list })));
    };

    // Current value: explicitly asked for, or implied by a bare sensor_type.
    if agg.as_deref() == Some("current") || (agg.is_none() && since.is_none() && until.is_none()) {
        let reading = state
            .sensor_storage
            .get_latest(&device_id, sensor_type)
            .await
            .map_err(storage_error)?;
        return Ok(Json(match reading {
            Some(r) => sensor_reading_json(&r),
            None => json!({ "readings": [] }),
        }));
    }

    // Aggregated by the store, so a wide window's rows never load here.
    if let Some(a) = agg.as_deref() {
        let summary = state
            .sensor_storage
            .aggregate(&device_id, sensor_type, since, until)
            .await
            .map_err(storage_error)?;
        // Keyed by the aggregate's name; an empty window gives `null`.
        let value = match a {
            "min" => summary.min,
            "max" => summary.max,
            _ => summary.avg,
        };
        let mut body = serde_json::Map::new();
        body.insert("device_id".into(), json!(device_id));
        body.insert("sensor_type".into(), json!(sensor_type));
        body.insert(a.into(), json!(value));
        body.insert("count".into(), json!(summary.count));
        body.insert("unit".into(), json!(summary.unit));
        return Ok(Json(Value::Object(body)));
    }

    let limit = params
        .limit
        .unwrap_or(SENSOR_HISTORY_MAX_LIMIT)
        .min(SENSOR_HISTORY_MAX_LIMIT);
    // One extra row tells a truncated window from one holding exactly `limit`.
    let mut readings = state
        .sensor_storage
        .get_history_limited(&device_id, sensor_type, since, until, limit + 1)
        .await
        .map_err(storage_error)?;
    let truncated = readings.len() > limit;
    readings.truncate(limit);
    let list: Vec<Value> = readings.iter().map(sensor_reading_json).collect();
    Ok(Json(
        json!({ "readings": list, "count": readings.len(), "truncated": truncated }),
    ))
}

fn parse_sensor_datetime(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    s.parse::<chrono::DateTime<chrono::Utc>>().ok().or_else(|| {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
            .ok()
            .map(|ndt| ndt.and_utc())
    })
}

/// Parses an optional sensor time param. Also takes a bare `YYYY-MM-DDTHH:MM:SS`, unlike
/// [`parse_rfc3339_param`], so existing requests keep working.
fn parse_sensor_time_param(
    field: &str,
    raw: Option<&str>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, (StatusCode, Json<Value>)> {
    match raw {
        None => Ok(None),
        Some(s) => parse_sensor_datetime(s).map(Some).ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(
                    json!({ "error": format!("invalid `{field}`: expected an RFC3339 timestamp") }),
                ),
            )
        }),
    }
}

// ── Activity query API ─────────────────────────────────────────────────────────

/// Row cap for the activity endpoints, whatever `limit` asks for.
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

/// `GET /api/v1/activity` — recent events, newest first. Never returns Secret events: the
/// store excludes them (so `limit` counts only visible ones) and the handler re-filters.
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

/// `DELETE /api/v1/activity` — "clear my activity"; with no filters it purges the whole log.
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

/// `GET /api/v1/activity/summary` — per-category counts over at most `ACTIVITY_MAX_LIMIT` events.
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

// ── The would-deny read surface ───────────────────────────────────────────────

/// Cap on audit events scanned per policy report; hitting it sets `truncated`.
const POLICY_REPORT_MAX_EVENTS: usize = 5000;

#[derive(serde::Deserialize)]
struct PolicyReportParams {
    /// Time window for the *events* half: "hour" | "day" (default) | "week".
    window: Option<String>,
}

/// `GET /api/v1/security/policy-report` — what would `enforce` break? Event log and
/// `POLICY_COUNTERS` stay separate: one is clearable, one resets on restart, and they overlap.
async fn policy_report(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<PolicyReportParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    use pond_core::security::ports::policy as pol;

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

    // Pre-seed every verdict so an empty window reports explicit zeros.
    let mut by_verdict: std::collections::BTreeMap<&str, u64> =
        pol::VERDICTS.iter().map(|v| (*v, 0u64)).collect();
    let mut scanned = 0usize;
    let mut unclassified = 0u64;
    let mut truncated = false;
    let mut events_available = false;

    if let Some(event_log) = state.event_log.as_ref() {
        events_available = true;
        // `EventQuery` can't filter by action or group, so both happen here; hence the cap.
        let rows = event_log
            .query(EventQuery {
                category: Some(EventCategory::Auth),
                since: Some(since),
                max_sensitivity: Some(PrivacySensitivity::Sensitive),
                limit: Some(POLICY_REPORT_MAX_EVENTS),
                ..Default::default()
            })
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "policy report query failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "policy report query failed" })),
                )
            })?;

        truncated = rows.len() >= POLICY_REPORT_MAX_EVENTS;

        for event in rows.iter().filter(|e| e.action == pol::AUDIT_ACTION) {
            scanned += 1;
            match event
                .attributes
                .get(pol::audit_attrs::VERDICT)
                .and_then(|v| match v {
                    pond_core::security::domain::event::AttributeValue::Text(s) => Some(s.as_str()),
                    _ => None,
                }) {
                // No verdict attribute: count it apart rather than assume `allow`.
                None => unclassified += 1,
                Some(v) => match by_verdict.get_mut(v) {
                    Some(slot) => *slot += 1,
                    None => unclassified += 1,
                },
            }
        }
    }

    let process = pol::POLICY_COUNTERS.snapshot();

    Ok(Json(json!({
        "window": window,
        "since": since.to_rfc3339(),
        "events": {
            "allow":      by_verdict["allow"],
            "would_deny": by_verdict["would_deny"],
            "deny":       by_verdict["deny"],
            "unclassified": unclassified,
            "scanned": scanned,
            "truncated": truncated,
            "available": events_available,
            "note": "durable across restarts; pruned on retention and erasable by DELETE /api/v1/activity",
        },
        "process": {
            "allow":      process.allow,
            "would_deny": process.would_deny,
            "deny":       process.deny,
            "counting_since": process.counting_since.to_rfc3339(),
            "note": "survives log pruning and DELETE /api/v1/activity; resets on restart",
        },
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
    // Publish after persisting, so the event carries its DB id.
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

/// Transcribes multipart audio via whisper.cpp. Deliberately unauthenticated (localhost dev
/// tool): add auth before exposing the server to the internet.
async fn transcribe(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Interrupt consolidation now, not at `/chat/stream`, so it frees the inference slot in time.
    state.note_user_activity().await;

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

    if let Some(transcribe_fn) = &state.transcribe_audio {
        let fn_clone = transcribe_fn.clone();
        let transcript = tokio::task::spawn_blocking(move || fn_clone(bytes))
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("transcription task panicked: {e}")})),
                )
            })?
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("transcription failed: {e}")})),
                )
            })?;
        return Ok(Json(json!({"text": transcript})));
    }

    let whisper_url = format!("{}/inference", state.whisper_url);
    // `voice_whisper_url` is free text, so audio may go off-box: gate it. `check_egress`, not
    // `begin`, so each utterance doesn't log an event and drown the privacy feed.
    pond_core::shared::services::egress::check_egress(&whisper_url).map_err(|denied| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": denied.to_string()})),
        )
    })?;
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

    // ── Transcribe ───────────────────────────────────────────────────────────
    // In-process first, like /transcribe: default builds don't spawn the whisper HTTP server.
    let raw_transcript = if let Some(transcribe_fn) = &state.transcribe_audio {
        let fn_clone = transcribe_fn.clone();
        tokio::task::spawn_blocking(move || fn_clone(bytes))
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("transcription task panicked: {e}")})),
                )
            })?
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("transcription failed: {e}")})),
                )
            })?
            .trim()
            .to_string()
    } else {
        let whisper_url = format!("{}/inference", state.whisper_url);
        // Gated for the same reason as in `transcribe`: the host is configurable.
        pond_core::shared::services::egress::check_egress(&whisper_url).map_err(|denied| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": denied.to_string()})),
            )
        })?;
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

        whisper_json["text"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string()
    };
    if raw_transcript.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "no speech detected in recording"})),
        ));
    }

    // Must normalize exactly as the wake-word detector does.
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

/// `DELETE /api/v1/voice/calibrate` — always safe: the detector falls back to the wake word.
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

    let (whisper_result, llamafile_result, ollama_result) = tokio::join!(
        probe(client, &state.whisper_url, 3),
        probe(client, "http://127.0.0.1:8080", 3),
        probe(client, "http://127.0.0.1:11434", 3),
    );

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

/// `GET /dev/test` — HTML dev test panel. **Never expose this to the internet.**
pub async fn dev_test_page() -> Html<&'static str> {
    Html(DEV_TEST_HTML)
}

/// `GET /dev/face` — webcam page for the face endpoints. **Never expose this to the internet.**
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

/// `POST /api/v1/test/speak` — speaks `{ "text" }` on the server device's own speaker.
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

/// GETs `url` for status and latency; gated since `whisper_url` is user-set (loopback passes).
async fn probe(client: &reqwest::Client, url: &str, timeout_secs: u64) -> Value {
    let t0 = std::time::Instant::now();
    if let Err(denied) = pond_core::shared::services::egress::check_egress(url) {
        return json!({
            "status": "refused",
            "url": url,
            "error": denied.to_string(),
        });
    }
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

/// Accepts both the `kind` format and the legacy `prompt` + `payload` one.
#[derive(Debug, serde::Deserialize)]
struct ApiCreateScheduleRequest {
    /// Optional: if absent, a UUID is generated.
    id: Option<String>,
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
    /// Fire once at `cron`'s next occurrence instead of recurring.
    #[serde(default)]
    once: bool,
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

    // An explicit kind can smuggle a rule in here; validate it as `/rules` does or it's a bypass.
    if let TaskKind::SensorTrigger(spec) = &kind {
        if let Some((status, body)) = rule_spec_rejection(spec) {
            return (status, Json(body));
        }
    }

    let req = CreateScheduleRequest {
        fire_at: None,
        once: api_req.once,
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
        fire_at: api_req.fire_at,
        once: api_req.once,
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
    /// Fire once at this instant instead of recurring.
    #[serde(default)]
    fire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Fire once at `cron`'s next occurrence instead of recurring.
    #[serde(default)]
    once: bool,
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

// ───────────────────────── Sensor-rule Handlers ─────────────────────
//
// A rule is a `TaskKind::SensorTrigger` schedule, fired through the scheduler's `run_now` (no
// second store or path). This surface adds only rule naming and spec validation.

/// A schedule's rule view, or `None` for a cron schedule. Every `/rules/{id}` handler checks
/// this, so `/rules` can't pause or delete a cron schedule by id.
fn rule_view(s: &Schedule) -> Option<Value> {
    let TaskKind::SensorTrigger(spec) = &s.kind else {
        return None;
    };
    Some(json!({
        "id": s.id,
        "name": s.label,
        "paused": s.paused,
        "currently_running": s.currently_running,
        "created_at": s.created_at,
        // The durable cooldown stamp: the only field that answers "why hasn't it fired".
        "last_fired": s.last_run,
        "cooldown_secs": spec.cooldown_secs,
        "source": spec.source,
        "condition": spec.condition,
        "actions": spec.actions,
    }))
}

/// Cron placeholder for event-fired rules; must match the marker the scheduler and MCP tools use.
const RULE_CRON: &str = "@event";

/// The 400 an unfit rule spec earns; every route that stores rules (incl. `/schedules`) uses it.
fn rule_spec_rejection(spec: &SensorTriggerSpec) -> Option<(StatusCode, Value)> {
    spec.validate().err().map(|rejected| {
        (
            StatusCode::BAD_REQUEST,
            json!({"error": rejected.to_string()}),
        )
    })
}

/// Fetch one rule, or the response that says why not.
async fn find_rule(
    scheduler: &Arc<dyn pond_core::user_data::ports::scheduler::SchedulerPort>,
    id: &str,
) -> Result<Schedule, (StatusCode, Json<Value>)> {
    let tasks = scheduler.list_tasks().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    tasks
        .into_iter()
        // `rule_view` is the single "is this a rule" test, so cron schedules stay unreachable here.
        .find(|t| t.id == id && rule_view(t).is_some())
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("rule '{id}' not found")})),
            )
        })
}

/// The scheduler is optional in `AppState`; every handler here needs it.
fn require_scheduler(
    state: &Arc<AppState>,
) -> Result<Arc<dyn pond_core::user_data::ports::scheduler::SchedulerPort>, (StatusCode, Json<Value>)>
{
    state.scheduler.clone().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "scheduler not configured"})),
    ))
}

/// `GET /api/v1/rules` — every sensor rule, cron schedules excluded.
async fn list_rules(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let scheduler = match require_scheduler(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match scheduler.list_tasks().await {
        Ok(tasks) => {
            let rules: Vec<Value> = tasks.iter().filter_map(rule_view).collect();
            (StatusCode::OK, Json(json!(rules)))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

/// Create/replace body.
#[derive(Debug, serde::Deserialize)]
struct ApiRuleRequest {
    /// Optional on create; a UUID is generated. Ignored on update.
    id: Option<String>,
    #[serde(alias = "label")]
    name: String,
    #[serde(flatten)]
    spec: SensorTriggerSpec,
}

/// `POST /api/v1/rules` — create a sensor rule.
async fn create_rule(
    State(state): State<Arc<AppState>>,
    result: Result<Json<ApiRuleRequest>, JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let scheduler = match require_scheduler(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Json(req) = match result {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
        }
    };
    if let Some((status, body)) = rule_spec_rejection(&req.spec) {
        return (status, Json(body));
    }

    // Duplicate ids and the cron are the scheduler's to reject, not this handler's.
    let create = CreateScheduleRequest {
        fire_at: None,
        once: false,
        id: req.id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        label: req.name,
        cron: RULE_CRON.to_string(),
        timezone: "UTC".to_string(),
        kind: TaskKind::SensorTrigger(req.spec),
    };
    match scheduler.create_task(create).await {
        Ok(task) => match rule_view(&task) {
            Some(v) => (StatusCode::CREATED, Json(v)),
            None => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "scheduler returned a non-rule for a rule create"})),
            ),
        },
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

/// `GET /api/v1/rules/{id}` — one rule.
async fn get_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let scheduler = match require_scheduler(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match find_rule(&scheduler, &id).await {
        Ok(rule) => match rule_view(&rule) {
            Some(v) => (StatusCode::OK, Json(v)),
            None => (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("rule '{id}' not found")})),
            ),
        },
        Err(r) => r,
    }
}

/// `PUT /api/v1/rules/{id}` — replace a rule's name and spec.
async fn update_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    result: Result<Json<ApiRuleRequest>, JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let scheduler = match require_scheduler(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Json(req) = match result {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
        }
    };
    // Resolve before validating: a cron schedule's id must 404, never be overwritten.
    if let Err(r) = find_rule(&scheduler, &id).await {
        return r;
    }
    if let Some((status, body)) = rule_spec_rejection(&req.spec) {
        return (status, Json(body));
    }

    let update = UpdateScheduleRequest {
        label: Some(req.name),
        cron: None,
        timezone: None,
        kind: Some(TaskKind::SensorTrigger(req.spec)),
        fire_at: None,
        once: false,
    };
    match scheduler.update_task(&id, update).await {
        Ok(task) => match rule_view(&task) {
            Some(v) => (StatusCode::OK, Json(v)),
            None => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "scheduler returned a non-rule for a rule update"})),
            ),
        },
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))),
    }
}

/// `DELETE /api/v1/rules/{id}` — remove a rule.
async fn delete_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let scheduler = match require_scheduler(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = find_rule(&scheduler, &id).await {
        return r;
    }
    match scheduler.delete_task(&id).await {
        Ok(()) => (StatusCode::OK, Json(json!({"deleted": id}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))),
    }
}

/// `POST /api/v1/rules/{id}/pause` — stop the rule firing without deleting it.
async fn pause_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let scheduler = match require_scheduler(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = find_rule(&scheduler, &id).await {
        return r;
    }
    match scheduler.pause_task(&id).await {
        Ok(()) => (StatusCode::OK, Json(json!({"paused": id}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))),
    }
}

/// `POST /api/v1/rules/{id}/resume` — let it fire again.
async fn resume_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let scheduler = match require_scheduler(&state) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = find_rule(&scheduler, &id).await {
        return r;
    }
    match scheduler.resume_task(&id).await {
        Ok(()) => (StatusCode::OK, Json(json!({"resumed": id}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))),
    }
}

#[derive(serde::Deserialize)]
struct NotificationStreamParams {
    device_id: Option<String>,
}

/// `GET /api/v1/notifications/stream?device_id=X` — foreground push for one device.
/// Drains its offline queue, then tails live events for it or `"broadcast"`; dedupe by `id`.
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

    // Not the chat `sse_semaphore`: phones hold these open indefinitely and would starve chat.
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
        let _permit = permit;

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

/// Tools whose listing gets `_meta.ui.resourceUri` pointing at their MCP App resource.
const TOOL_UI_RESOURCES: &[(&str, &str)] = &[(
    "giap-weather__get_current_weather",
    "ui://giap-weather/weather-card.html",
)];

/// `GET /api/v1/agent/tools` — list all MCP tools currently loaded by the agent.
async fn list_agent_tools(State(state): State<Arc<AppState>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(manager) = &state.extension_manager else {
        return Json(json!([])).into_response();
    };
    match manager.list_tools_detailed().await {
        Ok(tools) => {
            let enriched: Vec<Value> = tools
                .iter()
                .map(|tool| {
                    let full_name = format!("{}__{}", tool.extension, tool.name);
                    let mut obj = json!({
                        "extension": tool.extension,
                        "name": tool.name,
                        "description": tool.description,
                    });
                    for &(prefix, uri) in TOOL_UI_RESOURCES {
                        if full_name == prefix || full_name.ends_with(prefix) {
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

/// `GET /api/v1/mcp/resources?uri=…` — serves embedded MCP App HTML as a `ReadResourceResult`.
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
    /// MCP server prefix (e.g. "giap-device-control"); optional if `tool` contains "__".
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

fn qualify_tool_name(server: &str, tool: &str) -> String {
    if tool.contains("__") || server.is_empty() {
        tool.to_string()
    } else {
        format!("{server}__{tool}")
    }
}

/// Tools callable without an LLM turn; deny-by-default. No session reaches them, so identity
/// guards pass in audit mode. Open to any paired client or MCP App: read-only/narrow only.
const DIRECT_DISPATCH_ALLOWLIST: &[&str] = &[
    "giap-device-control__set_device_state",
    // Read-only; the Devices card's power label (`powerStateOf`) needs it.
    "giap-device-control__get_device_state",
    "giap-weather__get_current_weather",
    "giap-weather__get_weather_forecast",
];

/// Runs an allowlisted tool straight through the MCP registry, with no LLM turn.
async fn dispatch_tool_direct(
    state: &Arc<AppState>,
    qualified: &str,
    args: Value,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !DIRECT_DISPATCH_ALLOWLIST.contains(&qualified) {
        tracing::warn!(
            target: "giap::trace",
            kind = "direct_dispatch_refused",
            tool = qualified,
            "a tool outside the direct-dispatch allowlist was requested LLM-free"
        );
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": format!(
                    "`{qualified}` cannot be invoked without a chat turn. Only a small set of \
                     tools is reachable directly, because this path carries no caller identity."
                )
            })),
        ));
    }
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

/// `POST /api/v1/mcp/tools/call` — MCP Apps' `callServerTool()`, by qualified name.
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

/// `POST /api/v1/agent/chat/stream` — the Goose agentic loop, with MCP tools, streamed as SSE.
async fn agent_chat_stream(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use futures::stream::StreamExt;
    use pond_core::shared::domain::agent::AgentRequest;

    // Device identity comes from the principal, never the hand-parsed body.
    let device = proven_device(principal.as_ref());

    // Resets the inactivity clock and interrupts any background consolidation.
    state.note_user_activity().await;

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

    // A malformed `images` is ignored, like every other optional field on this route.
    let images: Vec<pond_core::models::domain::message::ImageAttachment> = body
        .get("images")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    if let Err(resp) = image_limit_response(&images) {
        return resp.into_response();
    }

    let agent = state.agent.clone();
    let storage = state.session_storage.clone();

    let stream = async_stream::stream! {
        let _permit = permit;

        // Fail closed: unreadable settings mean reasoning is not persisted.
        let persist_thinking = state
            .settings_repo
            .get()
            .await
            .map(|s| s.persist_thinking)
            .unwrap_or(false);

        // Session row first: scope resolution and the user message both depend on it.
        if storage.get_session(&session_id).await.is_err() {
            if let Err(e) = storage.create_session(session_id.clone()).await {
                yield Ok(Event::default().data(json!({"error": e.to_string()}).to_string()));
                return;
            }
        }

        // After the session row exists, and before `ChatService`: its default scope is Household,
        // so building it first would attribute extracted memory to the whole household.
        let turn_scope = resolve_turn_scope(&state, &session_id, &device).await;

        let mut chat_service = pond_core::shared::services::chat::ChatService::new(
            agent.clone(),
            session_id.clone(),
            storage.clone(),
        )
        .with_profile_scope(turn_scope.clone())
        .with_thinking(persist_thinking);
        // Extraction guarded exactly as in `/chat/stream`.
        if let (Some(ext), Some(svc)) =
            (state.memory_extractor.clone(), state.memory_extraction_service.clone())
        {
            chat_service = chat_service.with_memory_extraction(
                ext,
                svc,
                state.memory_repo.clone(),
            );
        }
        if let Some(event_log) = state.event_log.clone() {
            chat_service = chat_service.with_event_log(event_log);
        }

        if let Err(e) = chat_service.persist_user_message_with_images(&message, images.clone()).await {
            yield Ok(Event::default().data(json!({"error": format!("Failed to persist user message: {}", e)}).to_string()));
            return;
        }

        let user_message_for_extraction = message.clone();

        // Shared with `/chat/stream` so both routes fold engine events into a turn identically.
        let mut turn = TurnAccumulator::new(crate::thought_filter::ThoughtFilter::new());
        let request = AgentRequest {
            message,
            session_id: session_id.clone(),
            model_role: "task".to_string(),
            images,
            voice_mode: false,
            canvas_mode: false,
            profile_scope: turn_scope.clone(),
            profile_context: profile_context_for(&state, &turn_scope).await,
            tool_group_allowlist: None,
            warmup: false,
        };

        let mut agent_stream = match agent.chat_stream(request).await {
            Ok(s) => s,
            Err(e) => {
                let data = json!({"error": e.to_string()}).to_string();
                yield Ok(Event::default().data(data));
                return;
            }
        };

        while let Some(event_result) = agent_stream.next().await {
            match event_result {
                Ok(event) => {
                    match turn.absorb(event) {
                        StreamStep::Nothing => {}
                        StreamStep::Frame(data) => {
                            yield Ok::<Event, std::convert::Infallible>(Event::default().data(data));
                        }
                        StreamStep::Reasoning { frame, block } => {
                            // Always streamed; persisted only if the user opted in.
                            chat_service.record_thinking(block);
                            yield Ok::<Event, std::convert::Infallible>(Event::default().data(frame));
                        }
                        // Stats first, then `done`; this route closes on the engine's `Done`.
                        // `usage` is dropped: a `done` total would change a shape clients parse.
                        StreamStep::TurnComplete { stats, .. } => {
                            if let Some(s) = stats {
                                yield Ok::<Event, std::convert::Infallible>(
                                    Event::default().data(turn_stats_frame(&s))
                                );
                            }
                            yield Ok::<Event, std::convert::Infallible>(Event::default().data(
                                json!({"done": true, "session_id": session_id.clone()}).to_string()
                            ));
                        }
                    }
                    for thinking_content in turn.thought.take_thinking() {
                        let data = json!({"type": "thinking", "content": thinking_content}).to_string();
                        yield Ok::<Event, std::convert::Infallible>(Event::default().data(data));
                    }
                    // Execute Harmony-format tool-call envelopes captured by ThoughtFilter.
                    for body in turn.thought.take_tool_calls() {
                        if let Some((name, args)) = crate::thought_filter::parse_tool_envelope(&body) {
                            tracing::info!(tool = %name, "Executing text-based tool call (agent chat)");
                            let call_id = uuid::Uuid::new_v4().to_string();
                            let args_val: serde_json::Value = serde_json::from_str(&args).unwrap_or(json!({}));
                            yield Ok::<Event, std::convert::Infallible>(Event::default().data(
                                json!({"type": "tool_call", "tool": name.clone(), "id": call_id.clone(), "input": args_val}).to_string()
                            ));
                            let call_start = std::time::Instant::now();
                            let call_result = state.agent.call_tool(&session_id, &name, &args).await;
                            turn.note_fallback_tool(&name, call_start.elapsed());
                            match call_result {
                                Ok(result_text) => {
                                    let (clean, ui_hint) = extract_ui_hint(&result_text);
                                    yield Ok::<Event, std::convert::Infallible>(Event::default().data(
                                        tool_result_frame(&name, &call_id, &clean, ui_hint)
                                    ));
                                }
                                Err(e) => {
                                    yield Ok::<Event, std::convert::Infallible>(Event::default().data(
                                        tool_result_frame(&name, &call_id, &format!("Tool error: {e}"), None)
                                    ));
                                }
                            }
                        } else {
                            tracing::warn!("Unrecognised tool-call envelope: {body}");
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

        let tail = turn.thought.flush();
        if !tail.is_empty() {
            turn.full_text.push_str(&tail);
            let data = json!({"type": "text", "content": tail, "token": tail}).to_string();
            yield Ok::<Event, std::convert::Infallible>(Event::default().data(data));
        }

        // ── Persist assistant turn ──────────────────────────────────────────
        // One call for persist + extract, so a refactor can't drop extraction.
        let _ = chat_service.persist_assistant_turn_with_extraction(
            std::mem::take(&mut turn.tool_results),
            &turn.full_text,
            None,
            None,
            &user_message_for_extraction,
        ).await;
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

// ── Event log / telemetry ─────────────────────────────────────────────────────

/// `GET /api/v1/logs` — recent `event_log` rows; `level` is `INFO`, `WARN` or `ERROR`.
async fn list_logs(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(repo) = &state.operational_log else {
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
async fn export_logs_csv(State(state): State<Arc<AppState>>) -> axum::response::Response {
    use axum::response::IntoResponse;

    let Some(repo) = &state.operational_log else {
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
            if let Some(registry) = &state.tool_registry {
                registry.deregister_extension(&name).await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

/// `PATCH /api/v1/extensions/{name}` — `{"enabled": bool}`; applies to future Goose sessions.
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

/// `POST /api/v1/marketplace/{id}/install` — optional `{ "secrets": {…} }`; 428 lists missing ones.
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

    let secrets: std::collections::HashMap<String, String> = body
        .and_then(|b| {
            b.0.get("secrets")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
        })
        .unwrap_or_default();

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

    if let Some(repo) = &state.secret_repo {
        for (key, value) in &secrets {
            if let Err(e) = repo.set(key, value).await {
                tracing::warn!("Failed to store secret '{}': {e}", key);
            }
        }
    }

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
    env.insert(
        crate::oauth_callback::GIAP_SERVER_URL_ENV_KEY.to_string(),
        crate::oauth_callback::local_server_url(state.api_port),
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

/// `PUT /api/v1/secrets/{key}` — set a secret; body `{ "value": "…" }`.
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

/// Respawns a marketplace extension so it reads current secrets (stdio env is fixed at spawn).
/// Errs whenever it can't; starts even uninstalled ones, so whether to call it is caller policy.
async fn restart_extension_with_secrets(state: &AppState, ext_id: &str) -> Result<(), String> {
    let (Some(mgr), Some(mp), Some(secret_repo)) = (
        &state.extension_manager,
        &state.marketplace,
        &state.secret_repo,
    ) else {
        return Err("This build cannot start extensions: no extension manager is running.".into());
    };

    let ext = match mp.get_by_id(ext_id).await {
        Ok(Some(ext)) => ext,
        Ok(None) => return Err(format!("'{ext_id}' is not in the marketplace.")),
        Err(e) => return Err(format!("Could not look up '{ext_id}': {e}")),
    };

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
    env.insert(
        crate::oauth_callback::GIAP_SERVER_URL_ENV_KEY.to_string(),
        crate::oauth_callback::local_server_url(state.api_port),
    );

    // Best-effort: fails harmlessly when it wasn't running (normal on a fresh install).
    if let Err(e) = mgr.remove_extension(ext_id).await {
        tracing::debug!(
            extension = %ext_id,
            error = %e,
            "could not stop the extension before restarting it"
        );
    }

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
        Ok(_) => {
            tracing::info!(extension = %ext_id, "restarted the extension with fresh credentials");
            Ok(())
        }
        Err(e) => {
            tracing::warn!(
                extension = %ext_id,
                error = %e,
                "failed to restart the extension after its credentials changed"
            );
            Err(e.to_string())
        }
    }
}

/// `POST /api/v1/extensions/{name}/secrets` — bulk-set `{ "KEY": "value" }`; keys are global.
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

    // Restart only an installed, enabled extension: starting others is a side effect nobody asked
    // for. Report stored and restarted separately; the store has already succeeded.
    let should_restart =
        match &state.mcp_server_repo {
            Some(repo) => match repo.list().await {
                Ok(saved) => saved.iter().any(|s| s.name == name && s.enabled),
                Err(e) => return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "stored": secrets.len(),
                        "error": format!("Stored, but could not read installed extensions: {e}"),
                    })),
                )
                    .into_response(),
            },
            None => false,
        };

    let (restarted, restart_error) = if should_restart {
        match restart_extension_with_secrets(&state, &name).await {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e)),
        }
    } else {
        tracing::info!(
            extension = %name,
            "stored credentials for an extension that is not installed and enabled; \
             nothing to restart"
        );
        (false, None)
    };

    Json(json!({
        "stored": secrets.len(),
        "restarted": restarted,
        "restart_error": restart_error,
    }))
    .into_response()
}

// ── OAuth PKCE handlers ──────────────────────────────────────────────────────

/// Secret key for a provider's client ID override. Pass the canonical `provider.id`, never a
/// request field: authorize and token exchange must resolve the same app.
fn client_id_secret_key(provider_id: &str) -> String {
    format!("{}_CLIENT_ID", provider_id.to_uppercase())
}

/// `POST /api/v1/oauth/authorize` — starts PKCE; the client opens the returned `auth_url`.
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

    let client_id = if let Some(repo) = &state.secret_repo {
        let key = client_id_secret_key(&provider.id);
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

/// Escapes untrusted text (stderr, provider error bodies) for the OAuth result pages.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// `GET /api/v1/oauth/callback?code=…&state=…` — swaps the code for tokens and stores them.
async fn oauth_callback_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::{Html, IntoResponse};

    let code = params.get("code").cloned().unwrap_or_default();
    let state_nonce = params.get("state").cloned().unwrap_or_default();

    let session = {
        let mut sessions = state.oauth_state.write().await;
        sessions.remove(&state_nonce)
    };

    // Every terminal branch records its outcome for the UI's status poll.
    let fail = |reason: &str| {
        let outcomes = state.oauth_outcomes.clone();
        let nonce = state_nonce.clone();
        let reason = reason.to_string();
        async move {
            crate::oauth_callback::record_outcome(
                &outcomes,
                &nonce,
                crate::oauth_callback::FlowOutcome::Failed(reason),
            )
            .await;
        }
    };

    let session = match session {
        Some(s) => s,
        None => {
            fail("Invalid or expired authorization state. Start the sign-in again.").await;
            return Html(
                "<h1>Authorization failed</h1>\
                 <p>Invalid or expired state. Please try again.</p>"
                    .to_string(),
            )
            .into_response();
        }
    };

    let providers = pond_core::user_data::services::oauth_providers::builtin_oauth_providers();
    let provider = match providers.iter().find(|p| p.id == session.provider_id) {
        Some(p) => p,
        None => {
            fail("Unknown OAuth provider.").await;
            return Html("<h1>Authorization failed</h1><p>Unknown provider.</p>".to_string())
                .into_response();
        }
    };

    let client_id = if let Some(repo) = &state.secret_repo {
        let key = client_id_secret_key(&session.provider_id);
        repo.get(&key)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| provider.bundled_client_id.clone())
    } else {
        provider.bundled_client_id.clone()
    };

    // redirect_uri must exactly match the one sent in the authorize request.
    let redirect_uri = format!("http://127.0.0.1:{}/api/v1/oauth/callback", state.api_port);
    // Egress refusals go through `fail` so the UI polling `/oauth/status/{state}` sees why.
    let call = match pond_core::shared::services::egress::begin(&provider.token_url, "POST") {
        Ok(c) => c,
        Err(denied) => {
            let reason = denied.to_string();
            fail(&reason).await;
            return Html(format!(
                "<h1>Authorization failed</h1><p>{}</p>",
                html_escape(&reason)
            ))
            .into_response();
        }
    };
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
    call.finish(token_response.as_ref().ok().map(|r| r.status().as_u16()));

    match token_response {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();

            if let Some(repo) = &state.secret_repo {
                if let Some(access_token) = body["access_token"].as_str() {
                    let _ = repo.set(&provider.token_key, access_token).await;
                }
                if let Some(refresh_token) = body["refresh_token"].as_str() {
                    let _ = repo.set(&provider.refresh_key, refresh_token).await;
                }
            }

            tracing::info!(provider = %provider.id, "OAuth token exchange succeeded");

            // Restart the installing extension so its child process picks up the new tokens.
            let restart_error = match &session.extension_id {
                Some(ext_id) => restart_extension_with_secrets(&state, ext_id).await.err(),
                None => None,
            };

            // Tokens are saved anyway; never show "Connected" over a dead extension.
            if let Some(err) = restart_error {
                fail(&format!(
                    "Signed in, but the {} extension did not start: {}",
                    session.extension_id.as_deref().unwrap_or("linked"),
                    err
                ))
                .await;
                return Html(format!(
                    r#"<!DOCTYPE html>
<html><head><title>Authorization Incomplete</title>
<style>body{{font-family:system-ui;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#f8f9fa}}
.card{{max-width:40rem;padding:2rem;border-radius:12px;background:white;box-shadow:0 2px 8px rgba(0,0,0,0.1)}}
h1{{color:#f59e0b;margin:0 0 .5rem}}p{{color:#6b7280}}
pre{{white-space:pre-wrap;word-break:break-word;background:#f3f4f6;padding:1rem;border-radius:8px;color:#374151;font-size:.8rem}}</style></head>
<body><div class="card"><h1>Signed in to {}, but the extension did not start</h1>
<p>Your credentials were saved. The <code>{}</code> extension failed to launch, so it will not work yet.</p>
<pre>{}</pre></div></body></html>"#,
                    html_escape(&provider.display_name),
                    html_escape(session.extension_id.as_deref().unwrap_or("unknown")),
                    html_escape(&err),
                ))
                .into_response();
            }

            crate::oauth_callback::record_outcome(
                &state.oauth_outcomes,
                &state_nonce,
                crate::oauth_callback::FlowOutcome::Completed,
            )
            .await;

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
            fail(&format!("Token exchange failed: {error_body}")).await;
            Html(format!(
                "<h1>Authorization failed</h1>\
                 <p>Token exchange error. Please try again.</p>\
                 <pre>{}</pre>",
                html_escape(&error_body)
            ))
            .into_response()
        }
        Err(e) => {
            tracing::error!(provider = %provider.id, error = %e, "OAuth token exchange network error");
            fail(&format!("Could not reach the provider: {e}")).await;
            Html(format!(
                "<h1>Authorization failed</h1><p>Network error: {}</p>",
                html_escape(&e.to_string())
            ))
            .into_response()
        }
    }
}

/// `GET /api/v1/oauth/status/{state}` — How the flow with this `state` nonce ended.
/// The UI polls this, not the secret store: when re-authorising, the token key already exists.
async fn oauth_status_handler(
    State(state): State<Arc<AppState>>,
    Path(state_nonce): Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    if state.oauth_state.read().await.contains_key(&state_nonce) {
        return Json(json!({"status": "pending"})).into_response();
    }

    match crate::oauth_callback::peek_outcome(&state.oauth_outcomes, &state_nonce).await {
        Some(crate::oauth_callback::FlowOutcome::Completed) => {
            Json(json!({"status": "completed"})).into_response()
        }
        Some(crate::oauth_callback::FlowOutcome::Failed(error)) => {
            Json(json!({"status": "failed", "error": error})).into_response()
        }
        None => Json(json!({"status": "unknown"})).into_response(),
    }
}

/// `POST /api/v1/oauth/refresh` — Refresh an OAuth access token; body `{"provider": "spotify"}`.
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
        let key = client_id_secret_key(&provider.id);
        repo.get(&key)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| provider.bundled_client_id.clone())
    };

    let call = match pond_core::shared::services::egress::begin(&provider.token_url, "POST") {
        Ok(c) => c,
        Err(denied) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": denied.to_string()})),
            )
                .into_response()
        }
    };
    let sent = state
        .http_client
        .post(&provider.token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await;
    call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
    match sent {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            if let Some(access_token) = body["access_token"].as_str() {
                let _ = repo.set(&provider.token_key, access_token).await;
            }
            // Some providers rotate refresh tokens
            if let Some(new_refresh) = body["refresh_token"].as_str() {
                let _ = repo.set(&provider.refresh_key, new_refresh).await;
            }
            let new_access_token = body["access_token"].as_str().map(String::from);
            tracing::info!(provider = %provider_id, "OAuth token refresh succeeded");

            // Detached so the calling extension reads this response before being killed.
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

async fn refresh_spotify_access_token(state: &AppState) -> Option<String> {
    let repo = state.secret_repo.as_ref()?;
    let providers = pond_core::user_data::services::oauth_providers::builtin_oauth_providers();
    let provider = providers.iter().find(|p| p.id == "spotify")?;
    let refresh_token = repo.get(&provider.refresh_key).await.ok().flatten()?;
    let client_id = repo
        .get(&client_id_secret_key(&provider.id))
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| provider.bundled_client_id.clone());

    // Each hop is gated where it's made: one gate up front lets a copy-pasted retry slip past.
    let call = pond_core::shared::services::egress::begin(&provider.token_url, "POST").ok()?;
    let sent = state
        .http_client
        .post(&provider.token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await;
    call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
    let resp = sent.ok()?;
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

/// Maps to a `&'static str` so attacker-chosen methods can't mint unbounded event-store keys.
fn static_method_label(method: &reqwest::Method) -> &'static str {
    if method == reqwest::Method::GET {
        "GET"
    } else if method == reqwest::Method::PUT {
        "PUT"
    } else if method == reqwest::Method::POST {
        "POST"
    } else if method == reqwest::Method::DELETE {
        "DELETE"
    } else {
        "OTHER"
    }
}

/// Why a Spotify call failed; refusals stay distinct so the UI doesn't say "sign in again".
enum SpotifyUnavailable {
    /// No secret storage, or no stored token: Spotify was never connected.
    NotConnected,
    /// The network mode refused a hop. Carries the whole `EgressDenied` text.
    Refused(String),
}

/// Calls the Spotify Web API, refreshing the token and retrying once on a 401.
async fn spotify_api_call(
    state: &AppState,
    method: reqwest::Method,
    path: &str,
) -> Result<reqwest::Response, SpotifyUnavailable> {
    use SpotifyUnavailable::{NotConnected, Refused};

    let repo = state.secret_repo.as_ref().ok_or(NotConnected)?;
    let providers = pond_core::user_data::services::oauth_providers::builtin_oauth_providers();
    let provider = providers
        .iter()
        .find(|p| p.id == "spotify")
        .ok_or(NotConnected)?;
    let token = repo
        .get(&provider.token_key)
        .await
        .ok()
        .flatten()
        .ok_or(NotConnected)?;
    let url = format!("https://api.spotify.com/v1{path}");

    let method_label = static_method_label(&method);
    let call = pond_core::shared::services::egress::begin(&url, method_label)
        .map_err(|denied| Refused(denied.to_string()))?;
    let sent = state
        .http_client
        .request(method.clone(), &url)
        .bearer_auth(&token)
        .send()
        .await;
    call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
    let resp = sent.map_err(|_| NotConnected)?;

    if resp.status() != StatusCode::UNAUTHORIZED {
        return Ok(resp);
    }

    let refreshed = refresh_spotify_access_token(state)
        .await
        .ok_or(NotConnected)?;
    // The retry is its own request with a fresh credential, so it gets its own gate.
    let retry = pond_core::shared::services::egress::begin(&url, method_label)
        .map_err(|denied| Refused(denied.to_string()))?;
    let sent = state
        .http_client
        .request(method, &url)
        .bearer_auth(&refreshed)
        .send()
        .await;
    retry.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
    sent.map_err(|_| NotConnected)
}

/// Maps a failing Spotify status to a stable code plus text the dashboard shows verbatim.
/// 403: dev-mode Spotify apps serve only allowlisted accounts, yet others complete OAuth fine.
fn spotify_error_hint(status: StatusCode) -> (&'static str, &'static str) {
    match status {
        StatusCode::UNAUTHORIZED => (
            "unauthorized",
            "Spotify rejected the saved credentials. Sign in to Spotify again.",
        ),
        StatusCode::FORBIDDEN => (
            "forbidden",
            "This Spotify account is not authorised for the app GIAP signs in with. \
             Add it to that app's users in the Spotify developer dashboard, or set \
             your own SPOTIFY_CLIENT_ID and sign in again.",
        ),
        StatusCode::TOO_MANY_REQUESTS => (
            "rate_limited",
            "Spotify is rate-limiting requests. Playback should reappear shortly.",
        ),
        _ => (
            "unavailable",
            "Spotify did not return playback information.",
        ),
    }
}

/// `GET /api/v1/music/now-playing` — Spotify playback snapshot for the dashboard widget.
async fn music_now_playing_handler(State(state): State<Arc<AppState>>) -> axum::response::Response {
    use axum::response::IntoResponse;

    let resp = match spotify_api_call(&state, reqwest::Method::GET, "/me/player/currently-playing")
        .await
    {
        Ok(r) => r,
        Err(SpotifyUnavailable::NotConnected) => {
            return Json(json!({"connected": false})).into_response()
        }
        // Not "not connected": the widget should name the setting, not offer a useless sign-in.
        Err(SpotifyUnavailable::Refused(message)) => {
            return Json(json!({
                "connected": false,
                "error": "network_refused",
                "message": message,
            }))
            .into_response()
        }
    };

    let status = resp.status();

    // 204 is the only status that genuinely means "connected, nothing playing".
    if status == StatusCode::NO_CONTENT {
        return Json(json!({"connected": true, "playing": false})).into_response();
    }

    if !status.is_success() {
        // Any other status is a real failure; don't report it as an idle player.
        let (error, message) = spotify_error_hint(status);
        let body_preview = resp.text().await.unwrap_or_default();
        tracing::warn!(status = %status, error, body = %body_preview, "Spotify now-playing request failed");
        return Json(json!({
            "connected": true,
            "playing": false,
            "error": error,
            "message": message,
            // The widget stops polling after repeated 4XX; `error` can't tell 4XX from 5xx.
            "upstream_status": status.as_u16(),
        }))
        .into_response();
    }

    let mut body: serde_json::Value = resp.json().await.unwrap_or_default();

    // Spotify often omits `item` for episodes here; `/me/player` sometimes has it.
    if body["item"].is_null() {
        if let Ok(full_resp) = spotify_api_call(&state, reqwest::Method::GET, "/me/player").await {
            if full_resp.status().is_success() {
                if let Ok(full_body) = full_resp.json::<serde_json::Value>().await {
                    if !full_body["item"].is_null() {
                        body = full_body;
                    }
                }
            }
        }
    }
    if body["item"].is_null() {
        tracing::debug!(
            playing_type = body["currently_playing_type"].as_str().unwrap_or(""),
            "Spotify now-playing: item still null after the /me/player fallback"
        );
    }

    Json(now_playing_snapshot(&body)).into_response()
}

/// Shapes a Spotify playback body into the widget's JSON; pure so fixtures can pin it.
fn now_playing_snapshot(body: &serde_json::Value) -> serde_json::Value {
    let item = &body["item"];

    // `item`'s shape depends on `currently_playing_type`: episodes have no `artists` or `album`.
    let playing_type = body["currently_playing_type"].as_str().unwrap_or("");
    let (track, artist, album_art, duration_ms) = match playing_type {
        "episode" => (
            item["name"].as_str().unwrap_or(""),
            item["show"]["name"].as_str().unwrap_or(""),
            item["images"][0]["url"].as_str(),
            item["duration_ms"].as_i64().unwrap_or(0),
        ),
        // "track", "ad", "unknown" or absent: all may carry a track-shaped `item`, or none.
        _ => (
            item["name"].as_str().unwrap_or(""),
            item["artists"][0]["name"].as_str().unwrap_or(""),
            item["album"]["images"][0]["url"].as_str(),
            item["duration_ms"].as_i64().unwrap_or(0),
        ),
    };

    // Spotify may omit `item` on both endpoints; name what's known rather than show a blank.
    let is_playing = body["is_playing"].as_bool().unwrap_or(false);
    let track = if track.is_empty() && is_playing {
        match playing_type {
            "episode" => "Podcast episode",
            "ad" => "Advertisement",
            _ => "Something's playing",
        }
    } else {
        track
    };
    let artist = if artist.is_empty() && is_playing && item.is_null() {
        "Spotify didn't share the title"
    } else {
        artist
    };

    json!({
        "connected": true,
        "playing": is_playing,
        "track": track,
        "artist": artist,
        "album_art": album_art,
        "progress_ms": body["progress_ms"].as_i64().unwrap_or(0),
        "duration_ms": duration_ms,
    })
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

    let resp = match spotify_api_call(&state, method, path).await {
        Ok(r) => r,
        Err(SpotifyUnavailable::NotConnected) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "Spotify not connected"})),
            )
                .into_response()
        }
        Err(SpotifyUnavailable::Refused(message)) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": message, "code": "network_refused"})),
            )
                .into_response()
        }
    };

    match resp.status() {
        s if s.is_success() => Json(json!({"ok": true})).into_response(),
        StatusCode::NOT_FOUND => (
            StatusCode::CONFLICT,
            Json(json!({"error": "No active Spotify device. Open Spotify on a device first."})),
        )
            .into_response(),
        // Name the failure; don't report an authorisation problem as a generic outage.
        status => {
            let (error, message) = spotify_error_hint(status);
            tracing::warn!(status = %status, error, "Spotify control request failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": message, "code": error})),
            )
                .into_response()
        }
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
    /// Absent means "leave it alone", not "clear it": the desktop client sends only `content`.
    #[serde(default)]
    description: Option<String>,
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
    // Keep `is_system` (delete protection); `is_customized` makes the startup reseed skip it.
    let existing = repo.get(&name).await.ok().flatten();
    let existing_is_system = existing.as_ref().map(|t| t.is_system).unwrap_or(false);
    let description = req
        .description
        .or_else(|| existing.as_ref().map(|t| t.description.clone()))
        .unwrap_or_default();
    let template = PromptTemplate {
        name: name.clone(),
        content: req.content,
        description,
        is_system: existing_is_system,
        is_customized: true,
        // The forked-from version; stamping the current one would hide the newer-built-in notice.
        factory_version: existing.as_ref().map(|t| t.factory_version).unwrap_or(0),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    match repo.upsert(&template).await {
        // The saved row, not a status stub: the desktop client reads `content` from it.
        Ok(()) => Json(template).into_response(),
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
        is_customized: false,
        // A reset adopts the current built-in, which clears the "newer version" notice.
        factory_version: pond_core::user_data::domain::prompt_template::FACTORY_VERSION,
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
    match state
        .memory_repo
        .search_recent(&ProfileScope::Household, 50)
        .await
    {
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

#[derive(Deserialize)]
struct UpdateMemoryRequest {
    content: String,
}

/// `PUT /api/v1/memories/{id}` -- reword in place, keeping id, age, usage and vector.
async fn update_memory(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateMemoryRequest>,
) -> impl axum::response::IntoResponse {
    let content = body.content.trim();
    if content.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "a memory cannot be blank -- delete it instead"})),
        )
            .into_response();
    }
    match state.memory_repo.update_content(&id, content).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
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

/// POST /api/v1/memory/consolidate — run consolidation, streaming `ConsolidationEvent`s as SSE.
async fn start_consolidation(State(state): State<Arc<AppState>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    use pond_core::user_data::ports::memory_consolidator::ConsolidationEvent;

    let runner = match &state.consolidation_runner {
        Some(r) => r.clone(),
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Memory consolidation is not available in this build"})),
            )
                .into_response();
        }
    };

    // Read the live setting, not a startup snapshot: the toggle must apply without a restart.
    let enabled = state
        .settings_repo
        .get()
        .await
        .map(|s| s.memory_consolidation_enabled)
        .unwrap_or(false);
    if !enabled {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"error": "Memory consolidation is not enabled"})),
        )
            .into_response();
    }

    let cancel = tokio_util::sync::CancellationToken::new();
    *state.consolidation_cancel.write().await = Some(cancel.clone());

    // Subscribe BEFORE spawning so we don't miss the first event
    let mut rx = state.consolidation_event_tx.subscribe();

    tokio::spawn(runner(cancel));

    let stream = async_stream::stream! {
        while let Ok(event) = rx.recv().await {
            let json = serde_json::to_string(&event).unwrap_or_default();
            yield Ok::<_, Infallible>(Event::default().data(json));
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
    #[serde(default)]
    description: String,
    #[serde(default = "default_skill_icon")]
    icon: String,
    content: String,
}

fn default_skill_icon() -> String {
    "sparkles".to_string()
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
        description: req.description,
        icon: req.icon,
        content: req.content,
        active: true,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(e) = skill.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
    }
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
    name: Option<String>,
    description: Option<String>,
    icon: Option<String>,
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
        name: req.name.unwrap_or(existing.name),
        description: req.description.unwrap_or(existing.description),
        icon: req.icon.unwrap_or(existing.icon),
        content: req.content.unwrap_or(existing.content),
        active: req.active.unwrap_or(existing.active),
        created_at: existing.created_at,
    };
    if let Err(e) = updated.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
    }
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

/// A recipe parameter, as goose's own `RecipeParameter` shape.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
struct RecipeParameter {
    key: String,
    #[serde(default = "RecipeParameter::default_input_type")]
    input_type: String, // string | number | boolean | date | file | select
    #[serde(default = "RecipeParameter::default_requirement")]
    requirement: String, // required | optional | user_prompt
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    options: Option<Vec<String>>,
}

impl RecipeParameter {
    fn default_input_type() -> String {
        "string".to_string()
    }
    fn default_requirement() -> String {
        "optional".to_string()
    }
    fn is_required(&self) -> bool {
        self.requirement == "required" && self.default.is_none()
    }
}

/// A recipe extension entry, as goose's own `extensions:` shape.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
struct RecipeExtensionSpec {
    #[serde(rename = "type", default)]
    ext_type: String,
    name: String,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    bundled: Option<bool>,
}

/// The fields GIAP reads from a recipe's YAML. Unknown fields are allowed, not refused;
/// see [`RecipeYaml::warn_about_dropped_fields`].
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
struct RecipeYaml {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    parameters: Vec<RecipeParameter>,
    #[serde(default)]
    extensions: Vec<RecipeExtensionSpec>,
    #[serde(default)]
    activities: Vec<String>,
    /// Parsed only so `warn_about_dropped_fields` can flag them; GIAP doesn't execute either.
    #[serde(default)]
    sub_recipes: Option<Value>,
    #[serde(default)]
    response: Option<Value>,
}

impl RecipeYaml {
    fn parse(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Warns when `sub_recipes`/`response` get silently dropped; silence would read as support.
    fn warn_about_dropped_fields(&self, name: &str) {
        if self.sub_recipes.is_some() || self.response.is_some() {
            tracing::warn!(
                target: "giap::trace",
                kind = "recipe_fields_dropped",
                recipe = name,
                sub_recipes = self.sub_recipes.is_some(),
                response = self.response.is_some(),
                "this recipe declares fields GIAP does not execute"
            );
        }
    }
}

/// Maps goose extension names to GIAP tool-group prefixes; unknown names grant nothing.
fn recipe_extension_to_tool_group(name: &str) -> Option<&'static str> {
    match name {
        "weather" => Some("giap-weather"),
        "schedule" | "scheduler" => Some("giap-schedule"),
        "memory" => Some("giap-memory"),
        "device" | "developer" => Some("giap-device"),
        // Not `giap-matter`: that's the matter.js websocket protocol, not a tool group.
        "matter" | "home" => Some("giap-device-control"),
        _ => None,
    }
}

/// Recipe JSON plus its parsed YAML fields; unparseable YAML degrades to empty fields.
fn recipe_view_json(recipe: &AgentRecipe) -> Value {
    let mut value = json!(recipe);
    let parsed = RecipeYaml::parse(&recipe.yaml).unwrap_or_default();
    if let Some(obj) = value.as_object_mut() {
        obj.insert("title".to_string(), json!(parsed.title));
        obj.insert("parameters".to_string(), json!(parsed.parameters));
        obj.insert("extensions".to_string(), json!(parsed.extensions));
        obj.insert("activities".to_string(), json!(parsed.activities));
    }
    value
}

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
        Ok(recipes) => {
            let views: Vec<Value> = recipes.iter().map(recipe_view_json).collect();
            Json(json!(views)).into_response()
        }
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
        Ok(()) => (StatusCode::CREATED, Json(recipe_view_json(&recipe))).into_response(),
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
        Ok(()) => Json(recipe_view_json(&updated)).into_response(),
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
    /// Values for the recipe's declared `parameters:`. Keyed by `key`.
    #[serde(default)]
    parameters: Option<std::collections::HashMap<String, String>>,
}

/// Substitutes goose's `{{key}}` placeholders; recipe syntax has no logic, so plain replace.
fn substitute_recipe_params(
    text: &str,
    values: &std::collections::HashMap<String, String>,
) -> String {
    let mut out = text.to_string();
    for (key, value) in values {
        out = out.replace(&format!("{{{{{key}}}}}"), value);
    }
    out
}

/// Runs a recipe by name; the SSE matches `POST /api/v1/chat/stream` event-for-event.
async fn run_recipe(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    body: Option<Json<RunRecipeRequest>>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)>
{
    // The speaker is the device this caller's token was issued to, never the request body.
    let device = proven_device(principal.as_ref());

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

    let body = body.map(|Json(b)| b).unwrap_or_default();
    let supplied_params = body.parameters.clone().unwrap_or_default();

    let parsed = match RecipeYaml::parse(&recipe.yaml) {
        Ok(parsed) => {
            parsed.warn_about_dropped_fields(&name);
            parsed
        }
        Err(e) => {
            tracing::warn!(name = %name, error = %e, "failed to parse recipe YAML; using fallback prompt");
            RecipeYaml::default()
        }
    };

    // Refuse missing required params first: a half-substituted prompt is worse than a 400.
    let missing: Vec<&str> = parsed
        .parameters
        .iter()
        .filter(|p| p.is_required() && !supplied_params.contains_key(&p.key))
        .map(|p| p.key.as_str())
        .collect();
    if !missing.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "missing required parameters",
                "missing": missing,
            })),
        ));
    }

    let mut effective_params = std::collections::HashMap::new();
    for p in &parsed.parameters {
        if let Some(default) = &p.default {
            effective_params.insert(p.key.clone(), default.clone());
        }
    }
    effective_params.extend(supplied_params);

    let raw_prompt = parsed
        .prompt
        .clone()
        .or(parsed.instructions.clone())
        .unwrap_or_else(|| format!("Run routine: {}", name));
    let prompt = substitute_recipe_params(&raw_prompt, &effective_params);

    // Narrowing only: unknown names are dropped with a warning, not refused.
    let tool_group_allowlist = if parsed.extensions.is_empty() {
        None
    } else {
        let mut groups: Vec<String> = Vec::new();
        for ext in &parsed.extensions {
            match recipe_extension_to_tool_group(&ext.name) {
                Some(group) => groups.push(group.to_string()),
                None => tracing::warn!(
                    name = %name,
                    extension = %ext.name,
                    "recipe extension has no GIAP tool-group equivalent; dropped"
                ),
            }
        }
        Some(groups)
    };

    // Resets the inactivity clock and interrupts any background consolidation.
    state.note_user_activity().await;

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

    let chat_req = ChatRequest {
        // Schedule-driven: no client will come back to resume it.
        resumable: false,
        session_id: body.session_id,
        message: prompt,
        images: Vec::new(),
        voice_mode: body.voice_mode,
        canvas_mode: body.canvas_mode,
        tool_group_allowlist,
    };

    Ok(chat_stream_inner(state, permit, chat_req, device))
}

// ───────────────────────── Face Biometrics ──────────────────────────────────
// Without `bbox` ("x,y,w,h" source pixels) the adapter center-crops; fine only for headshots.

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

/// POST /api/v1/faces/register — enroll a face (multipart); 3+ samples per profile advised.
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

/// POST /api/v1/faces/identify — best-matching profile above the cosine-similarity threshold.
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

/// GET /api/v1/faces/profile/:profile_id — enrollment metadata only; never the raw vector.
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

/// GET /api/v1/faces/models — on-disk status of each slot's preferred and fallback model.
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

/// GET /api/v1/faces/debug/pairwise — pairwise cosines plus a verdict on embedding collapse.
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

/// GET /api/v1/faces/debug/eval — FAR/FRR threshold sweep over stored pairs, for calibration.
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
        // First FAR≈FRR crossover: the EER-ish reference point the UI draws.
        if crossover.is_none() {
            if let Some(prev) = curve.last() {
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

/// Reads a burst: repeated `image` fields plus one optional `bbox` shared by all frames.
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
    // ~80 ms per frame on Jetson: 12 frames keeps the worst case under a second.
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

/// POST /api/v1/faces/identify-burst — multi-frame consensus, so one lucky frame can't match.
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

    let mut per_frame: Vec<Value> = Vec::with_capacity(n);
    let mut votes: std::collections::HashMap<String, (u32, f32)> = std::collections::HashMap::new(); // profile_id → (count, sum_confidence)
    let mut no_face_count = 0_u32;

    // Liveness: a held-up photo or screen gives near-identical embeddings and no landmark jitter.
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
        // info! on purpose: operators tune liveness thresholds from these numbers.
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
    // With one enrolled profile the matcher's runner-up/open-set checks are no-ops, so a burst
    // must be unanimous, clear per-frame and mean floors, and stay within the no-face budget.
    let suspicious = liveness.as_ref().map(|r| r.suspicious).unwrap_or(false);
    let threshold = face.match_threshold();

    // Tuned on w600k_r50: live frames score 0.85–0.92, photos 0.72–0.78; +0.10 splits them.
    let single_frame_margin = env_or("POND_FACE_BURST_FRAME_MARGIN", 0.10_f32);
    let mean_margin = env_or("POND_FACE_BURST_MEAN_MARGIN", 0.12_f32);
    let suspicious_extra = env_or("POND_FACE_BURST_SUSPICIOUS_MARGIN", 0.05_f32);
    let no_face_budget = env_or("POND_FACE_BURST_NOFACE_BUDGET", 0.20_f32);

    let frame_floor =
        threshold + single_frame_margin + if suspicious { suspicious_extra } else { 0.0 };
    let mean_floor = threshold + mean_margin + if suspicious { suspicious_extra } else { 0.0 };

    // Every face-bearing frame must agree; a single frame degrades to single-shot.
    let face_bearing = n as u32 - no_face_count;
    let no_face_ratio = if n == 0 {
        1.0
    } else {
        no_face_count as f32 / n as f32
    };

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
            // Keep the would-be winner so the UI can say "almost matched X".
            (false, Some(pid.clone()), Some(*mean), *count)
        }
        None => (false, None, None, 0),
    };

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

/// Burst liveness: `hard_reject` = almost surely a spoof; `suspicious` = tighten consensus.
struct LivenessReport {
    hard_reject: bool,
    suspicious: bool,
    mean_inter_cos: f32,
    landmark_motion: f32,
    eye_ratio_spread: f32,
    /// Mean non-rigid landmark displacement (px) between frames; a waved photo moves rigidly.
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

/// Liveness metrics from the burst's existing landmarks and embeddings (no extra inference).
fn compute_liveness_report(
    embeddings: &[Vec<f32>],
    landmarks: &[pond_core::user_data::domain::face_recognition::FaceLandmarks],
) -> LivenessReport {
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

    // Inter-eye distance spread: live faces vary ~1.5–6 % over 5 frames, a phone photo < 0.8 %.
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

    // Diff-motion floor 0.60 px: phone-screen photos with hand jitter leak ~0.3 px non-rigid.
    // Cosine ceiling 0.9994: live bursts reach 0.9992, screen replays ≥ 0.9995 (same pixels).
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

    // Any two: a live face may fail one gate (a still moment); a photo fails three or more.
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

/// POST /api/v1/faces/enroll-quality — would this frame enroll? Checks only; persists nothing.
async fn enroll_quality_handler(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(face_unavailable)?;
    let (profile_id, image, bbox) = read_face_multipart(multipart).await?;

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

    let existing: Vec<pond_core::user_data::domain::face_recognition::FaceEmbedding> =
        match profile_id.as_deref() {
            Some(pid) => face.list_embeddings(pid).await.unwrap_or_default(),
            None => Vec::new(),
        };

    let existing_count = existing.len();
    let self_consistency = if existing.len() >= 2 {
        // Mean pairwise cosine; ≥ 0.70 is "same person" for ArcFace-aligned 112×112 crops.
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

    // Same env var and default as the matcher's minimum in `sqlite_face_recognition`.
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

/// GET /api/v1/faces/profile/:profile_id/threshold — the override, or null if global applies.
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

/// DELETE /api/v1/users/:profile_id/biometrics — forget a member's biometric data.
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
        // `voice_prints_deleted` joins this once voice prints are stored.
    })))
}

// ── Proactive proposals ──────────────────────────────────────────────────────
// Proposals are `drafts` rows (`origin = 'proactive'`) sharing their ownership rule; approving
// executes nothing. Decide reads via `get_live` so user-staged drafts keep the audited MCP path.

/// The caller as exactly one member ([`ProposalAudience::from_scope`]); `Household` and `Guest`
/// are refused, so an unidentified session gets 403. Reads and decisions share this one door.
async fn proposal_caller(
    state: &Arc<AppState>,
    session_id: &str,
    device: &ProvenDevice,
) -> Result<(ProfileScope, ProposalAudience), (StatusCode, Json<Value>)> {
    let scope = resolve_turn_scope(state, session_id, device).await;
    match ProposalAudience::from_scope(&scope) {
        Ok(audience) => Ok((scope, audience)),
        Err(e) => {
            tracing::info!(
                target: "giap::trace",
                kind = "proposal_caller_unaddressable",
                session_id,
                reason = %e,
                "a proposal route refused a caller that is not one household member"
            );
            Err((
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": e.to_string(),
                    "hint": "proposals are addressed to one household member; bind this \
                             session to a member before reading or deciding one",
                })),
            ))
        }
    }
}

/// Stand-in for an injected repo, built from `AppState`'s pool; the swap stays a one-line change.
fn proposal_repo(state: &Arc<AppState>) -> pond_infra::sqlite_proposal::SqliteProposalRepository {
    pond_infra::sqlite_proposal::SqliteProposalRepository::new(state.db.system.clone())
}

/// Same stand-in as [`proposal_repo`], for the `drafts` table proposals live in.
fn draft_repo(state: &Arc<AppState>) -> pond_infra::sqlite_draft::SqliteDraftRepository {
    pond_infra::sqlite_draft::SqliteDraftRepository::new(state.db.system.clone())
}

/// The wire shape of a proposal, built by hand so `summary` (a method) is included.
/// `rationale` stays a separate key: showing only `summary` must not pass as showing the reason.
fn proposal_json(p: &pond_core::user_data::domain::proposal::Proposal) -> Value {
    let trigger = p.trigger();
    json!({
        "id": p.id(),
        "summary": p.summary(),
        "rationale": p.rationale(),
        "confidence": p.confidence(),
        "profile_id": p.audience().profile_id(),
        "created_at": p.created_at().to_rfc3339(),
        "expires_at": p.expires_at().to_rfc3339(),
        "proposed_action": p.proposed_action(),
        "trigger": {
            "kind": trigger.kind(),
            "source_id": trigger.source_id(),
            "signal": trigger.signal(),
            "observed_at": trigger.observed_at().to_rfc3339(),
        },
    })
}

/// Which session is asking; never defaulted, since a default would resolve to `Household`.
#[derive(Deserialize)]
struct ProposalCallerQuery {
    session_id: String,
}

/// `GET /api/v1/proposals?session_id=X` — this member's live proposals, newest first.
/// No list-all route by design (its only caller is a broadcast); expiry is filtered in SQL.
async fn list_proposals(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    Query(query): Query<ProposalCallerQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Device from the principal only, so a query string can't widen who the caller is.
    let device = proven_device(principal.as_ref());
    let (_scope, audience) = proposal_caller(&state, &query.session_id, &device).await?;

    let proposals = proposal_repo(&state)
        .list_live_for(audience.profile_id(), chrono::Utc::now())
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "could not list proposals");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "could not read proposals"})),
            )
        })?;

    Ok(Json(json!({
        "profile_id": audience.profile_id(),
        "proposals": proposals.iter().map(proposal_json).collect::<Vec<_>>(),
    })))
}

/// No default variant: an unreadable decision must be a 400, never an approval.
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ProposalDecision {
    Approve,
    Reject,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecideProposalRequest {
    session_id: String,
    decision: ProposalDecision,
}

/// `POST /api/v1/proposals/{id}/decide` — approve or reject; approving executes nothing.
async fn decide_proposal(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    body: Result<Json<DecideProposalRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Device from the principal; a body `device_id` is a 400 (`deny_unknown_fields`).
    let device = proven_device(principal.as_ref());
    let Json(request) = body.map_err(|_| bad_body())?;
    let (scope, _audience) = proposal_caller(&state, &request.session_id, &device).await?;

    // Expired, decided, user-staged draft and absent all get the same 404 on purpose.
    let proposal = proposal_repo(&state)
        .get_live(&id, chrono::Utc::now())
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, proposal = %id, "could not read a proposal");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "could not read the proposal"})),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": "no live proposal with that id",
                })),
            )
        })?;

    // `PROPOSAL_SESSION_ID` is the sentinel session `SqliteProposalRepository::save` writes.
    if let Err(reason) = is_draft_decision_permitted(
        Some(&scope),
        &request.session_id,
        Some(proposal.audience().profile_id()),
        PROPOSAL_SESSION_ID,
    ) {
        tracing::warn!(
            target: "giap::trace",
            kind = "proposal_decision_refused",
            proposal = %id,
            reason,
            "a proposal decision was refused"
        );
        return Err((StatusCode::FORBIDDEN, Json(json!({"error": reason}))));
    }

    let status = match request.decision {
        ProposalDecision::Approve => DraftStatus::Approved,
        ProposalDecision::Reject => DraftStatus::Rejected,
    };
    draft_repo(&state)
        .update_status(&id, status.clone())
        .await
        .map_err(|e| {
            // Migration 0041's trigger may veto the transition: a refusal (409), not a failure.
            tracing::warn!(error = %e, proposal = %id, "a proposal decision was not applied");
            (
                StatusCode::CONFLICT,
                Json(json!({"error": "the proposal could not be moved to that status"})),
            )
        })?;

    Ok(Json(json!({
        "id": id,
        "status": status.to_string(),
        // Said out loud because the word "approved" invites the other reading.
        "executed": false,
    })))
}

/// Resolves whose turn this is, once, at the edge; every failure narrows rather than widens.
/// `device` is a [`ProvenDevice`] because a paired device outranks every other identity proof.
async fn resolve_turn_scope(
    state: &Arc<AppState>,
    session_id: &str,
    device: &ProvenDevice,
) -> ProfileScope {
    // `rung` takes the `Result` so a failed read can't be flattened into "unattributed".
    let device_rung = match device.id() {
        None => DeviceRung::NoDevice,
        Some(device_id) => device.rung(device_attribution(state).device_profile(device_id).await),
    };
    match &device_rung {
        DeviceRung::Unavailable(why) => tracing::warn!(
            error = %why,
            session_id,
            "could not read this device's household member; treating the speaker as \
             unidentified rather than assuming one"
        ),
        DeviceRung::Member(profile_id) => tracing::debug!(
            target: "giap::trace",
            kind = "turn_device_identified",
            session_id,
            profile_id = %profile_id,
            "the paired device this turn arrived on belongs to a household member"
        ),
        DeviceRung::NoDevice | DeviceRung::Unattributed => {}
    }

    let identity = state
        .session_storage
        .get_session_identity(session_id)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                session_id,
                "could not read session identity; treating the speaker as unidentified"
            );
            SessionIdentity::unknown()
        });

    let household_has_multiple_members = match state.profile_repo.list().await {
        Ok(profiles) => profiles.len() > 1,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "could not count household members; assuming more than one"
            );
            true
        }
    };

    identity_resolution::resolve(&identity_resolution::ResolutionInputs {
        // Only `Member` is `Some`; the other rungs, a failed read included, fall through.
        paired_device_profile: device_rung.profile_id(),
        session: &identity,
        household_has_multiple_members,
    })
    .scope
}

/// The device the *pond* proved; every turn-resolving handler must get its device here.
fn proven_device(
    principal: Option<&axum::Extension<pond_core::security::ports::policy::Principal>>,
) -> ProvenDevice {
    match principal {
        Some(axum::Extension(principal)) => ProvenDevice::from_principal(principal),
        None => ProvenDevice::none(),
    }
}

// ── Connecting a source ────────────────────────────────────────────────────

/// Same stand-in as [`proposal_repo`]; takes the redactor because storage redacts on the way in.
fn context_repo(state: &Arc<AppState>) -> pond_infra::sqlite_context::SqliteContextRepository {
    // A fresh `RuleRedactor` equals `main.rs`'s only while it is stateless and unconfigured.
    pond_infra::sqlite_context::SqliteContextRepository::new(
        state.db.system.clone(),
        std::sync::Arc::new(pond_infra::rule_redactor::RuleRedactor::new()),
    )
}

#[derive(serde::Deserialize)]
struct ConnectSourceRequest {
    /// Kinds whose connector hasn't landed are refused via `SourceKind::availability`.
    kind: String,
    /// A sensor's `device_id` or a camera's `camera_id`; a value naming no device never ingests.
    provider: String,
    /// The caller's conversation; the owner is resolved from it and the proven device.
    session_id: String,
    /// Required for account kinds (else it never syncs); refused for on-pond kinds.
    #[serde(default)]
    credentials: Option<ConnectCredentials>,
}

/// App-password sign-in for an account source. Never echoed back.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectCredentials {
    username: String,
    password: String,
    /// Only for the self-hosted presets (`nextcloud`, `custom`).
    #[serde(default)]
    base_url: Option<String>,
}

/// The source owner: resolved, never supplied, and fixed for good (migration 0044).
/// `Guest` is refused: a guest who could create a source would write into a member's corpus.
async fn context_source_owner(
    state: &Arc<AppState>,
    session_id: &str,
    device: &ProvenDevice,
) -> Result<String, (StatusCode, Json<Value>)> {
    let scope = resolve_turn_scope(state, session_id, device).await;
    if let Some(id) = scope.owner_id() {
        return Ok(id.to_string());
    }

    // A one-member `Household` means that member; with more, picking would go by row order.
    // Residual risk: an unidentified caller on an unattributed device connects as the member.
    if matches!(scope, ProfileScope::Household) {
        let members: Vec<String> = state
            .profile_repo
            .list()
            .await
            .map(|profiles| profiles.into_iter().map(|p| p.id).collect())
            .unwrap_or_default();
        if let Some(only) =
            pond_core::user_data::services::member_attribution::sole_member(&members)
        {
            return Ok(only);
        }
    }

    Err((
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "a context source belongs to one household member, and this caller could \
                      not be resolved to one. Identify yourself in this conversation, or pair \
                      this device to a member.",
            "scope": format!("{scope:?}"),
        })),
    ))
}

/// `POST /api/v1/context/sources` -- connect a sensor or camera as personal context.
async fn connect_context_source(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    Json(body): Json<ConnectSourceRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    use pond_core::context::domain::{ContextSource, SourceKind, SourceParts, SourceStatus};

    let device = proven_device(principal.as_ref());
    let owner = context_source_owner(&state, &body.session_id, &device).await?;

    let Some(kind) = SourceKind::parse(&body.kind) else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("unknown source kind: {}", body.kind)})),
        ));
    };
    // Refuse here: a source with no connector would look connected and never produce anything.
    let availability = kind.availability();
    if availability != pond_core::context::domain::SourceAvailability::Landed {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": availability.refusal(), "kind": kind.as_str()})),
        ));
    }

    // Refuse unconnectable providers now: Google CalDAV needs OAuth 2.0 and rejects Basic auth.
    if kind == SourceKind::Calendar {
        if let Some(provider) =
            pond_adapters_caldav::CalDavProvider::from_stored(body.provider.trim(), Some("x"))
        {
            if !provider.is_connectable() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": provider.setup_hint(),
                        "provider": body.provider.trim(),
                    })),
                ));
            }
        }
    }

    let now = chrono::Utc::now();
    // Deterministic, so reconnecting updates. Account kinds include the owner, or two members'
    // Google calendars would collide on `calendar:google`.
    let id = if kind.needs_credentials() {
        format!("{}:{}:{}", kind.as_str(), body.provider.trim(), owner)
    } else {
        format!("{}:{}", kind.as_str(), body.provider.trim())
    };

    // Credentials first: a failed secret write must not leave a source that can't sync.
    let secret_ref = match (kind.needs_credentials(), body.credentials.as_ref()) {
        (true, None) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "this source signs in to an account, so it needs a username and an \
                              app password",
                })),
            ))
        }
        (false, Some(_)) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "this source is already on the pond and needs no sign-in details",
                })),
            ))
        }
        (false, None) => None,
        (true, Some(creds)) => {
            // Refuse: the alternatives are a source that never syncs or an unencrypted password.
            let Some(secrets) = state.secret_repo.as_ref() else {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({
                        "error": "this pond has no encrypted secret store, so it cannot hold \
                                  an account password",
                    })),
                ));
            };
            let key = pond_core::context::domain::secret_key_for(&id);
            let blob = json!({
                "username": creds.username,
                "password": creds.password,
                "base_url": creds.base_url,
            })
            .to_string();
            secrets.set(&key, &blob).await.map_err(|e| {
                tracing::warn!(error = %e, "could not store calendar credentials");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "could not store the sign-in details"})),
                )
            })?;
            Some(key)
        }
    };
    let source = ContextSource::from_parts(SourceParts {
        id: id.clone(),
        kind,
        provider: body.provider.trim().to_string(),
        profile_id: owner.clone(),
        scopes: Vec::new(),
        cursor: None,
        last_sync: None,
        status: SourceStatus::Connected,
        secret_ref,
        created_at: now,
    })
    .map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
    })?;

    context_repo(&state)
        .upsert_source(&source)
        .await
        .map_err(|e| {
            // Migration 0044's trigger refuses owner/kind changes; that's a 409, not a 500.
            let moved_owner = e.to_string().contains("cannot change owner")
                || e.to_string().contains("cannot change kind");
            if moved_owner {
                (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": "this source already exists and belongs to somebody else, or is a \
                                  different kind. Disconnect it first; its items go with it.",
                    })),
                )
            } else {
                tracing::warn!(error = %e, "could not store a context source");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "could not store the source"})),
                )
            }
        })?;

    Ok(Json(
        json!({"id": id, "kind": kind.as_str(), "profile_id": owner}),
    ))
}

/// `GET /api/v1/context/items?session_id=X&q=…` -- what the pond has read, scoped in SQL.
/// Keyword search, not semantic: a cosine ranking buries an exact match.
async fn list_context_items(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let session_id = params.get("session_id").cloned().unwrap_or_default();
    let device = proven_device(principal.as_ref());
    let scope = resolve_turn_scope(&state, &session_id, &device).await;

    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(1, 500);

    let query = params.get("q").map(|q| q.trim()).unwrap_or_default();
    let repo = context_repo(&state);
    let items = if query.is_empty() {
        repo.recent_items(&scope, limit).await
    } else {
        // The store matches terms; a whole phrase would match only that exact string.
        let terms: Vec<String> = query.split_whitespace().map(|t| t.to_string()).collect();
        repo.search_items(&terms, &scope, limit).await
    };

    let items = items.map_err(|e| {
        tracing::warn!(error = %e, "could not read context items");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "could not read what the pond has collected"})),
        )
    })?;

    Ok(Json(json!({
        "items": items
            .iter()
            .map(|i| json!({
                "id": i.id(),
                "source_id": i.source_id(),
                "source_kind": i.source_kind().as_str(),
                "kind": i.kind().as_str(),
                "title": i.title(),
                "body": i.body(),
                "occurred_at": i.occurred_at().to_rfc3339(),
                "participants": i.participants(),
                // Whether retrieval can reach it; usually why search missed an item.
                "searchable": i.embedding().is_some(),
            }))
            .collect::<Vec<_>>()
    })))
}

/// `POST /api/v1/context/sync` -- pull every connected account now and report what it found.
async fn sync_context_sources(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(syncer) = state.account_sync.as_ref() else {
        // Not a zero: that would read as a working account with no news.
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "this pond cannot reach connected accounts -- it has no encrypted \
                          secret store to read their sign-in details from",
            })),
        ));
    };
    match syncer.sync_now().await {
        Ok(summary) => Ok(Json(json!(summary))),
        Err(e) => {
            tracing::warn!(error = %e, "a requested account sync failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "the sync could not run"})),
            ))
        }
    }
}

/// `GET /api/v1/context/sources?session_id=X` -- the sources this caller may see.
async fn list_context_sources(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    Query(query): Query<ProposalCallerQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let device = proven_device(principal.as_ref());
    // Scoped in the query (`Guest` sees nothing), not filtered afterwards.
    let scope = resolve_turn_scope(&state, &query.session_id, &device).await;
    let sources = context_repo(&state)
        .list_sources(&scope)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "could not list context sources");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "could not read sources"})),
            )
        })?;

    // One grouped query; on failure keep the list and lose only the counts.
    let stats = context_repo(&state)
        .item_stats_by_source()
        .await
        .unwrap_or_default();

    Ok(Json(json!({
        "sources": sources
            .iter()
            .map(|s| {
                let stat = stats.iter().find(|st| st.source_id == s.id());
                json!({
                "id": s.id(),
                "kind": s.kind().as_str(),
                "provider": s.provider(),
                "profile_id": s.profile_id(),
                "status": s.status().as_str(),
                // `null` = never synced, which must not look like "checked, found nothing".
                "last_sync": s.last_sync().map(|t| t.to_rfc3339()),
                "needs_credentials": s.kind().needs_credentials(),
                // Reported apart: the unindexed gap explains a search coming up short.
                "items": stat.map(|st| st.items).unwrap_or(0),
                "awaiting_index": stat.map(|st| st.awaiting_index).unwrap_or(0),
            })
            })
            .collect::<Vec<_>>()
    })))
}

/// `DELETE /api/v1/context/sources/{id}?session_id=X` -- disconnect, and say how many items went.
async fn disconnect_context_source(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    Path(id): Path<String>,
    Query(query): Query<ProposalCallerQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let device = proven_device(principal.as_ref());
    let scope = resolve_turn_scope(&state, &query.session_id, &device).await;
    let removed = context_repo(&state)
        .disconnect_source(&id, &scope)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "could not disconnect a context source");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "could not disconnect the source"})),
            )
        })?;

    Ok(Json(json!({"id": id, "items_removed": removed})))
}

// ── The coverage number leaves the process ─────────────────────────────────
// Expose `IndexHealth` over HTTP, plus a rebuild that forces the re-embed.

/// Shared by both routes below so they can't tell different stories about the same pond.
const NO_VECTOR_INDEX: &str = "this pond has no vector index, so nothing is embedded and \
                               retrieval falls back to recency";
const NO_EMBEDDING_MODEL: &str = "no embedding model is configured, so nothing has been indexed \
                                  and retrieval falls back to recency";

/// Coverage as a fraction, or `null` for `0/0`, which is neither 0% nor 100%.
fn index_coverage(indexed: u64, rows: u64) -> Option<f64> {
    (rows > 0).then(|| indexed as f64 / rows as f64)
}

/// Every IANA zone with today's offset (computed per request: a cached one breaks at DST).
async fn list_time_zones() -> Json<Value> {
    use pond_core::user_data::services::location::zone_catalogue;
    let now = chrono::Utc::now();
    let zones: Vec<Value> = zone_catalogue(now)
        .into_iter()
        .map(|c| json!({ "zone": c.zone, "offset": c.offset, "place": c.place }))
        .collect();
    Json(json!({ "zones": zones }))
}

/// What the client already knows, offered to the cascade as hints.
#[derive(Debug, Default, serde::Deserialize)]
struct DetectLocationRequest {
    /// The zone this device is set to — `Intl.DateTimeFormat()` on the desktop.
    #[serde(default)]
    system_zone: Option<String>,
    /// A name the household typed, which beats anything derived.
    #[serde(default)]
    typed_name: Option<String>,
    /// Coordinates a real browser answered with, when one did.
    #[serde(default)]
    latitude: Option<f64>,
    #[serde(default)]
    longitude: Option<f64>,
}

/// Works out where this pond is, cheapest source first. The address-revealing network source is
/// deliberately unwired; a geocode of a place NAME tells the far end nothing about who asked.
async fn detect_location(
    State(state): State<Arc<AppState>>,
    body: Option<Json<DetectLocationRequest>>,
) -> Json<Value> {
    use pond_core::user_data::ports::place_lookup::PlaceLookup;
    use pond_core::user_data::services::place_detection::{detect, Hints};

    let req = body.map(|Json(b)| b).unwrap_or_default();
    let geocoder = pond_adapters_weather::Geocoder::new(state.http_client.clone());
    let lookup: &dyn PlaceLookup = &geocoder;

    let device_coords = match (req.latitude, req.longitude) {
        (Some(lat), Some(lon)) => Some((lat, lon)),
        _ => None,
    };

    let found = detect(
        Hints {
            system_zone: req.system_zone.as_deref(),
            typed_name: req.typed_name.as_deref(),
            device_coords,
        },
        Some(lookup),
        // See the note above: not wired.
        None,
    )
    .await;

    Json(json!({
        "name": found.name,
        "latitude": found.latitude,
        "longitude": found.longitude,
        "timezone": found.timezone,
        "source": found.source.as_str(),
        // Whether this is a fact or a good guess, so the screen can say which.
        "certain": found.source.is_certain(),
        "has_coordinates": found.has_coordinates(),
        "note": found.note,
    }))
}

async fn context_index_health(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let no_index = |reason: &str| {
        Json(json!({
            "indexed": false,
            "reason": reason,
            "model_id": Value::Null,
            "dims": Value::Null,
            "coverage": Value::Null,
            "corpora": [],
        }))
    };

    let Some(index) = state.vector_index.as_ref() else {
        return Ok(no_index(NO_VECTOR_INDEX));
    };
    // No embedder, no model to ask about; "all missing" would look like a wiped index instead.
    let Some(embedder) = state.embedding_provider.as_ref() else {
        return Ok(no_index(NO_EMBEDDING_MODEL));
    };

    let model_id = embedder.model_id();
    let health = index.health(&model_id).await.map_err(|e| {
        tracing::warn!(error = %e, "could not read personal-context index health");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "could not read index health"})),
        )
    })?;

    // Summed per corpus like the other totals, so the overall fraction agrees with the rows.
    let rows: u64 = health.per_corpus.iter().map(|c| c.rows).sum();

    Ok(Json(json!({
        "indexed": true,
        "model_id": model_id,
        "dims": embedder.dimensions(),
        "rows": rows,
        "matching": health.matching,
        "mismatched": health.mismatched,
        "missing": health.missing,
        "coverage": index_coverage(health.matching, rows),
        "corpora": health
            .per_corpus
            .iter()
            .map(|c| json!({
                "corpus": c.corpus.as_str(),
                "rows": c.rows,
                "source_rows": c.source_rows,
                "indexed_rows": c.indexed_rows,
                "missing_rows": c.missing_rows,
                "mismatched": c.mismatched,
                "coverage": index_coverage(c.indexed_rows, c.rows),
                // A predicate excludes every source row; no amount of embedding repairs that.
                "structurally_excluded": c.rows == 0 && c.source_rows > 0,
            }))
            .collect::<Vec<_>>(),
    })))
}

/// `POST /api/v1/context/index/rebuild` -- empty the index so the maintenance sweep refills it.
/// The sweep fills only absent vectors, so this repairs a changed embedder; nothing is lost.
async fn rebuild_context_index(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    use pond_core::context::vector_index::Corpus;

    if state.vector_index.is_none() {
        return Ok(Json(json!({
            "indexed": false,
            "reason": NO_VECTOR_INDEX,
            "cleared": 0,
            "corpora": [],
        })));
    }

    let db_error = |e: sqlx::Error| {
        tracing::warn!(error = %e, "could not clear the personal-context index");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "could not clear the index"})),
        )
    };

    // Raw SQL: `VectorIndex` can only drop one row or prune orphans, neither of which empties it.
    let mut tx = state.db.vectors.begin().await.map_err(db_error)?;

    // Count before the delete, in the same transaction, so a concurrent sweep can't skew it.
    let counted: Vec<(String, i64)> =
        sqlx::query_as("SELECT corpus, COUNT(*) FROM vectors GROUP BY corpus")
            .fetch_all(&mut *tx)
            .await
            .map_err(db_error)?;
    let cleared = sqlx::query("DELETE FROM vectors")
        .execute(&mut *tx)
        .await
        .map_err(db_error)?
        .rows_affected();
    tx.commit().await.map_err(db_error)?;

    // List every corpus even at zero; the total is the DELETE's, so rows of retired corpora count.
    let corpora: Vec<Value> = Corpus::ALL
        .iter()
        .map(|corpus| {
            let n = counted
                .iter()
                .find(|(name, _)| name == corpus.as_str())
                .map(|(_, n)| (*n).max(0) as u64)
                .unwrap_or(0);
            json!({"corpus": corpus.as_str(), "cleared": n})
        })
        .collect();

    tracing::info!(cleared, "personal-context index cleared for rebuild");

    // Wake the idle-gated sweep (the user isn't idle). `notify_one` stores a permit, so a sweep
    // mid-pass still runs a fresh pass afterwards.
    let refilling = match state.index_reindex.as_ref() {
        Some(notify) => {
            notify.notify_one();
            true
        }
        // No sweep to wake; the next process rebuilds from the emptied table.
        None => false,
    };

    Ok(Json(json!({
        "indexed": true,
        "cleared": cleared,
        "refilling": refilling,
        "corpora": corpora,
    })))
}

/// Same stand-in as [`proposal_repo`], for the device-attribution repository.
fn device_attribution(
    state: &Arc<AppState>,
) -> pond_infra::sqlite_device_attribution::SqliteDeviceAttribution {
    pond_infra::sqlite_device_attribution::SqliteDeviceAttribution::new(state.db.system.clone())
}

/// The speaking member's prompt particulars; `Household` uses the primary profile, `Guest` none.
/// Name, birthday and language are Owner-scoped; `atypical_speech` is not.
async fn profile_context_for(
    state: &Arc<AppState>,
    scope: &ProfileScope,
) -> Option<ProfileContext> {
    // `attributed`: the turn is one member's, so their particulars may be stated.
    let (profile_id, attributed) = match scope {
        ProfileScope::Owner(id) => (id.clone(), true),
        ProfileScope::Household => {
            let settings = state.settings_repo.get().await.ok()?;
            (
                settings.primary_profile_id.filter(|id| !id.is_empty())?,
                false,
            )
        }
        ProfileScope::Guest => return None,
    };

    let profile = state.profile_repo.get(&profile_id).await.ok().flatten()?;
    Some(particulars_for(attributed, &profile.preferences))
}

/// The pure half of [`profile_context_for`]: what the prompt may state from these preferences.
/// Keys are the contract with `PATCH /api/v1/profiles/{id}`; a camelCase key is ignored.
fn particulars_for(
    attributed: bool,
    prefs: &std::collections::HashMap<String, String>,
) -> ProfileContext {
    ProfileContext {
        // Owner-scoped: stated only when the turn is attributed to this person.
        preferred_name: attributed
            .then(|| prefs.get("preferred_name").cloned())
            .flatten(),
        birthday: attributed.then(|| prefs.get("birthday").cloned()).flatten(),
        language: attributed.then(|| prefs.get("language").cloned()).flatten(),
        // Household-safe: an accommodation, not a disclosure.
        atypical_speech: prefs
            .get("accessibility_atypical_speech")
            .map(|v| v == "true")
            .unwrap_or(false),
    }
}

/// Only a genuinely missing session is a 404; anything else is a server fault, not "no session".
fn identity_write_error(e: SessionStorageError) -> (StatusCode, Json<Value>) {
    let status = match e {
        SessionStorageError::SessionNotFound(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({"error": e.to_string()})))
}

/// POST /api/v1/sessions/:session_id/identify-user — wake-on-face; a match binds the session.
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

    // Face is the weakest binding source: it must never override a device or explicit binding.
    let mut bound = false;
    if result.identified {
        if let Some(pid) = result.profile_id.clone() {
            let proposed = SessionIdentity {
                profile_id: Some(pid),
                source: IdentificationSource::Face,
                confidence: result.confidence,
            };
            // One atomic conditional write: read-then-write would let a weaker late write win.
            bound = state
                .session_storage
                .set_session_identity_if_stronger(&session_id, &proposed)
                .await
                .map_err(identity_write_error)?;
        }
    }

    Ok(Json(json!({
        "session_id": session_id,
        "identified": result.identified,
        "profile_id": result.profile_id,
        "confidence": result.confidence,
        "threshold":  face.match_threshold(),
        "bound":      bound,
    })))
}

/// Body of `PUT /api/v1/sessions/:session_id/user`.
#[derive(Debug, Deserialize)]
struct SetSessionUserRequest {
    profile_id: String,
}

/// Decides and records whether a caller may bind a session to a member at `Explicit` strength.
/// The mode is read on every call; in `audit` a refusal still proceeds, flagged `would_deny`.
async fn evaluate_identity_assertion(
    state: &Arc<AppState>,
    principal: Option<pond_core::security::ports::policy::Principal>,
    asserted_profile_id: &str,
) -> anyhow::Result<pond_core::security::ports::policy::PolicyDecision> {
    use pond_core::security::ports::policy as pol;

    let mode = pol::PolicyMode::parse(&state.settings_repo.get().await?.security_policy_mode);
    if mode == pol::PolicyMode::Off {
        return Ok(pol::PolicyDecision::permit(mode));
    }

    // No principal is a wiring fault, not anonymity: fall back to least privilege.
    let principal = principal.unwrap_or_else(pol::Principal::internal);

    let decision = if pol::is_identity_assertion_proven(&principal, asserted_profile_id) {
        pol::PolicyDecision::permit(mode)
    } else {
        pol::PolicyDecision::refuse(mode, pol::REASON_UNPROVEN_IDENTITY)
    };

    // Tally here, unconditionally, not inside an `audit` impl. Counters survive log clearing and
    // the event log survives restarts, so `GET /security/policy-report` reports both.
    pol::POLICY_COUNTERS.record(&decision);
    if let Some(policy) = &state.security_policy {
        policy
            .audit(
                &principal,
                // A plain verb: the report groups on it, and the verdict is its own attribute.
                "identify_session",
                pol::scopes::SESSION,
                &decision,
            )
            .await;
    }
    if decision.would_deny() {
        tracing::warn!(
            target: "giap::trace",
            kind = "policy_would_deny",
            scope = pol::scopes::SESSION,
            reason = decision.denied_reason,
            "security policy would have denied this in enforce mode"
        );
    }
    Ok(decision)
}

async fn set_session_user_handler(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    Json(req): Json<SetSessionUserRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if req.profile_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "profile_id is required"})),
        ));
    }

    // ── Identity assertion goes through the policy ──────────────────────────
    // Binds a body-supplied `profile_id` at Explicit strength, so it goes through the policy.
    let decision = evaluate_identity_assertion(&state, principal.map(|e| e.0), &req.profile_id)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "could not evaluate the security policy"})),
            )
        })?;
    if !decision.allowed {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "not permitted to identify this session as that member",
                "reason": decision.denied_reason,
            })),
        ));
    }

    let proposed = SessionIdentity {
        profile_id: Some(req.profile_id.clone()),
        source: IdentificationSource::Explicit,
        // Not 1.0: a number would invite averaging this claim against a face score.
        confidence: None,
    };

    // Atomic, for the same reason as the face path above.
    let bound = state
        .session_storage
        .set_session_identity_if_stronger(&session_id, &proposed)
        .await
        .map_err(identity_write_error)?;

    if !bound {
        // Read after the refusal so this reports the identity that actually won.
        let existing = state
            .session_storage
            .get_session_identity(&session_id)
            .await
            .unwrap_or_else(|_| SessionIdentity::unknown());
        return Ok(Json(json!({
            "session_id": session_id,
            "profile_id": existing.profile_id,
            "identification_source": existing.source.as_str(),
            "bound": false,
            "reason": "a stronger identification already holds this session",
        })));
    }

    Ok(Json(json!({
        "session_id": session_id,
        "profile_id": req.profile_id,
        "identification_source": IdentificationSource::Explicit.as_str(),
        "bound": true,
    })))
}

/// GET /api/v1/sessions/:session_id/user — the bound profile plus the evidence behind it.
async fn get_session_user_handler(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let identity = state
        .session_storage
        .get_session_identity(&session_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;
    Ok(Json(json!({
        "session_id": session_id,
        "profile_id": identity.profile_id,
        "identification_source": identity.source.as_str(),
        "confidence": identity.confidence,
    })))
}

/// DELETE /api/v1/sessions/:session_id/user — release the binding; narrowing needs no check.
async fn clear_session_user_handler(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Defaulting is safe: this only shapes the response; the release runs regardless.
    let had_binding = state
        .session_storage
        .get_session_identity(&session_id)
        .await
        .map(|i| i.profile_id.is_some())
        .unwrap_or(false);

    state
        .session_storage
        .set_session_identity(&session_id, &SessionIdentity::unknown())
        .await
        .map_err(identity_write_error)?;

    Ok(Json(json!({
        "session_id": session_id,
        "cleared":    had_binding,
    })))
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {

    /// Tailnet address detection; the range check stops the probe's LAN answer passing as remote.
    mod tailnet_range {
        use super::super::is_tailnet_v4;
        use std::net::Ipv4Addr;

        #[test]
        fn accepts_the_whole_cgnat_range_tailscale_assigns_from() {
            for ip in [
                Ipv4Addr::new(100, 64, 0, 0),
                Ipv4Addr::new(100, 100, 100, 100),
                Ipv4Addr::new(100, 127, 255, 255),
            ] {
                assert!(is_tailnet_v4(ip), "{ip} is inside 100.64.0.0/10");
            }
        }

        #[test]
        fn rejects_the_neighbours_of_that_range() {
            // Ordinary public space: an off-by-one would publish somebody else's address.
            for ip in [
                Ipv4Addr::new(100, 63, 255, 255),
                Ipv4Addr::new(100, 128, 0, 0),
            ] {
                assert!(!is_tailnet_v4(ip), "{ip} is outside 100.64.0.0/10");
            }
        }

        #[test]
        fn rejects_the_lan_addresses_the_probe_returns_without_a_tailnet() {
            for ip in [
                Ipv4Addr::new(192, 168, 1, 11),
                Ipv4Addr::new(10, 0, 0, 5),
                Ipv4Addr::new(172, 16, 4, 2),
                Ipv4Addr::LOCALHOST,
            ] {
                assert!(!is_tailnet_v4(ip), "{ip} is not a tailnet address");
            }
        }
    }

    /// Whisper `.bin` files have no header, so facts come from whisper.cpp's naming convention.
    mod whisper_names {
        use super::super::whisper_facts_from_name;

        #[test]
        fn reads_size_and_language_off_the_published_convention() {
            assert_eq!(
                whisper_facts_from_name("ggml-base.en"),
                (Some("en".into()), Some("base".into()))
            );
            assert_eq!(
                whisper_facts_from_name("ggml-large-v3-turbo"),
                (Some("multilingual".into()), Some("large".into()))
            );
            assert_eq!(
                whisper_facts_from_name("ggml-tiny"),
                (Some("multilingual".into()), Some("tiny".into()))
            );
            assert_eq!(
                whisper_facts_from_name("ggml-small.en"),
                (Some("en".into()), Some("small".into()))
            );
        }

        /// A stray `.bin` labelled as whisper would appear under Listening with a false size.
        #[test]
        fn claims_nothing_about_a_file_it_does_not_recognise() {
            assert_eq!(whisper_facts_from_name("some-random-weights"), (None, None));
            assert_eq!(whisper_facts_from_name(""), (None, None));
        }
    }
    use super::*;

    mod now_playing_snapshot_tests {
        use super::now_playing_snapshot;
        use serde_json::json;

        #[test]
        fn a_normal_track_reads_its_own_fields() {
            let body = json!({
                "is_playing": true,
                "progress_ms": 1000,
                "currently_playing_type": "track",
                "item": {
                    "name": "Weightless",
                    "artists": [{"name": "Marconi Union"}],
                    "album": {"images": [{"url": "https://example.com/art.jpg"}]},
                    "duration_ms": 500000,
                },
            });
            let snap = now_playing_snapshot(&body);
            assert_eq!(snap["track"], "Weightless");
            assert_eq!(snap["artist"], "Marconi Union");
            assert_eq!(snap["album_art"], "https://example.com/art.jpg");
            assert_eq!(snap["duration_ms"], 500000);
        }

        #[test]
        fn an_episode_with_a_real_item_reads_the_show_not_an_artist() {
            let body = json!({
                "is_playing": true,
                "progress_ms": 1000,
                "currently_playing_type": "episode",
                "item": {
                    "name": "Episode 42: The Question",
                    "show": {"name": "Hitchhiker's Weekly"},
                    "images": [{"url": "https://example.com/cover.jpg"}],
                    "duration_ms": 3600000,
                },
            });
            let snap = now_playing_snapshot(&body);
            assert_eq!(snap["track"], "Episode 42: The Question");
            assert_eq!(snap["artist"], "Hitchhiker's Weekly");
            assert_eq!(snap["album_art"], "https://example.com/cover.jpg");
        }

        /// Spotify really sends `item: null` for a playing episode; it isn't a parse failure.
        #[test]
        fn an_episode_with_a_null_item_says_so_instead_of_going_blank() {
            let body = json!({
                "is_playing": true,
                "progress_ms": 297926,
                "currently_playing_type": "episode",
                "item": null,
            });
            let snap = now_playing_snapshot(&body);
            assert_eq!(snap["track"], "Podcast episode");
            assert_eq!(snap["artist"], "Spotify didn't share the title");
            assert_eq!(snap["album_art"], serde_json::Value::Null);
            assert_eq!(snap["playing"], true);
        }

        #[test]
        fn an_ad_with_a_null_item_is_named_as_an_ad() {
            let body = json!({
                "is_playing": true,
                "progress_ms": 0,
                "currently_playing_type": "ad",
                "item": null,
            });
            let snap = now_playing_snapshot(&body);
            assert_eq!(snap["track"], "Advertisement");
        }

        #[test]
        fn a_paused_null_item_stays_empty_rather_than_inventing_a_label() {
            let body = json!({
                "is_playing": false,
                "progress_ms": 267736,
                "currently_playing_type": "episode",
                "item": null,
            });
            let snap = now_playing_snapshot(&body);
            assert_eq!(snap["track"], "");
            assert_eq!(snap["artist"], "");
            assert_eq!(snap["playing"], false);
        }
    }

    // ── direct tool dispatch allowlist ───────────────────────────

    /// Tools direct dispatch must never expose: it has no caller identity to check them against.
    /// List only tools that exist; an entry for a removed tool asserts nothing.
    const MUST_NEVER_BE_DIRECTLY_DISPATCHABLE: &[&str] = &[
        "giap-memory__recall_memories",
        "giap-memory__save_memory",
        "giap-schedule__create_schedule",
        // Any paired client or MCP App iframe could start it, with no `DelegationAuthority`.
        "giap-orchestrator__delegate",
    ];

    #[test]
    fn the_direct_dispatch_allowlist_holds_nothing_that_decides_executes_or_reads_memory() {
        for tool in MUST_NEVER_BE_DIRECTLY_DISPATCHABLE {
            assert!(
                !DIRECT_DISPATCH_ALLOWLIST.contains(tool),
                "{tool} is reachable without a chat turn, so without a caller"
            );
        }
    }

    #[test]
    fn the_allowlist_is_fully_qualified_so_a_bare_name_can_never_match() {
        // With an empty `server`, `qualify_tool_name` returns the bare name.
        for tool in DIRECT_DISPATCH_ALLOWLIST {
            assert!(
                tool.contains("__"),
                "{tool} is not server-qualified, so an unqualified request matches it"
            );
        }
    }

    #[test]
    fn the_hub_can_still_actuate_a_device() {
        // hubStore.ts and Rooms.tsx post {server, tool}, not a qualified name.
        let qualified = qualify_tool_name("giap-device-control", "set_device_state");
        assert!(DIRECT_DISPATCH_ALLOWLIST.contains(&qualified.as_str()));
    }

    // ── sensor rules ─────────────────────────────────────────────

    mod rules {
        use super::*;
        use pond_core::user_data::domain::schedule::{
            SensorTriggerSpec, TriggerAction, TriggerCondition, TriggerSource, TriggerSourceKind,
        };

        fn spec() -> SensorTriggerSpec {
            SensorTriggerSpec {
                source: TriggerSource {
                    kind: TriggerSourceKind::Sensor,
                    device_id: Some("backyard-pir".into()),
                    signal: Some("motion".into()),
                },
                condition: TriggerCondition::default(),
                actions: vec![TriggerAction::Notify {
                    title: "Motion".into(),
                    body: "Backyard".into(),
                }],
                cooldown_secs: 120,
            }
        }

        fn schedule(id: &str, kind: TaskKind) -> Schedule {
            Schedule {
                fire_at: None,
                id: id.into(),
                label: format!("label of {id}"),
                cron: RULE_CRON.into(),
                timezone: "UTC".into(),
                kind,
                paused: false,
                currently_running: false,
                last_run: None,
                next_run: None,
                created_at: chrono::Utc::now(),
            }
        }

        #[test]
        fn a_cron_schedule_is_not_a_rule() {
            // Projection only; `find_rule`'s `rule_view(t).is_some()` filter is covered by
            // `tests/rules_surface_test.rs` through the router.
            let backup = schedule(
                "nightly-backup",
                TaskKind::AgentPrompt {
                    prompt: "back up".into(),
                },
            );
            let leaked = "a cron schedule projected as a rule: /rules/{id} would \
                 let a caller pause or delete the household's nightly backup by \
                 guessing its id";
            assert!(rule_view(&backup).is_none(), "{leaked}");
            assert!(
                rule_view(&schedule(
                    "hook",
                    TaskKind::Webhook {
                        webhook_url: "https://example.com".into()
                    }
                ))
                .is_none(),
                "{leaked}"
            );
            // Vacuity control: a real rule projects, so the `None`s above are about the kind.
            assert!(rule_view(&schedule("r", TaskKind::SensorTrigger(spec()))).is_some());
        }

        #[test]
        fn the_rule_view_reports_the_durable_cooldown_stamp() {
            // `last_fired` is the only field here that answers "why hasn't my rule fired?".
            let fired_at = chrono::Utc::now() - chrono::Duration::seconds(30);
            let mut rule = schedule("r", TaskKind::SensorTrigger(spec()));
            rule.last_run = Some(fired_at);

            let v = rule_view(&rule).expect("a sensor rule projects");
            assert_eq!(v["last_fired"], json!(fired_at));
            assert_eq!(v["cooldown_secs"], json!(120));
            assert_eq!(v["name"], json!("label of r"));
            // The cron and timezone a rule never uses stay off the surface.
            assert!(v.get("cron").is_none(), "{v}");
            assert!(v.get("timezone").is_none(), "{v}");
        }

        #[test]
        fn the_create_body_is_the_rule_itself_with_no_cron() {
            let body = json!({
                "name": "Backyard motion after sunset",
                "source": {"kind": "sensor", "device_id": "backyard-pir", "signal": "motion"},
                "condition": {"after": "18:30", "before": "06:00"},
                "actions": [{"type": "notify", "title": "Motion", "body": "Backyard"}]
            });
            let req: ApiRuleRequest = serde_json::from_value(body).expect("flattened spec parses");
            assert_eq!(req.name, "Backyard motion after sunset");
            assert!(req.id.is_none());
            // The domain default, not 0 (no debounce on a flapping PIR).
            assert_eq!(req.spec.cooldown_secs, 60);
            assert_eq!(req.spec.condition.after.as_deref(), Some("18:30"));
            assert!(req.spec.validate().is_ok());
        }

        /// This file, for the order tripwire below (real coverage: `tests/rules_surface_test.rs`).
        const ROUTES_SRC: &str = include_str!("routes.rs");

        /// The source of one handler: from its signature to the next `async fn`.
        fn handler_body(signature: &str) -> &'static str {
            let start = ROUTES_SRC
                .find(signature)
                .unwrap_or_else(|| panic!("{signature} is gone from routes.rs"));
            let rest = &ROUTES_SRC[start + signature.len()..];
            let end = rest
                .find("\nasync fn ")
                .unwrap_or_else(|| panic!("{signature} is the last handler in the file"));
            &rest[..end]
        }

        #[test]
        fn the_slicer_returns_one_handler_and_not_the_file() {
            // Vacuity control: a whole-file slice would let the ordering guard pass by accident.
            let body = handler_body("async fn create_rule(");
            assert!(body.len() < ROUTES_SRC.len() / 4, "the slice is too big");
            assert!(
                body.contains("scheduler returned a non-rule for a rule create"),
                "the slice is not create_rule's body"
            );
            assert!(
                !body.contains("scheduler returned a non-rule for a rule update"),
                "the slice ran on into update_rule"
            );
        }

        /// The span from the rejection call to the store call; guard and control share it.
        fn rejection_arm(signature: &str, store_call: &str) -> &'static str {
            let body = handler_body(signature);
            let check = body
                .find("rule_spec_rejection(")
                .unwrap_or_else(|| panic!("{signature} no longer validates the rule spec"));
            let store = body
                .find(store_call)
                .unwrap_or_else(|| panic!("{signature} no longer calls {store_call}"));
            &body[check..store]
        }

        #[test]
        fn every_door_that_stores_a_rule_validates_first() {
            for (signature, store_call) in [
                ("async fn create_rule(", "create_task("),
                ("async fn update_rule(", "update_task("),
                // Rules can still be created here by naming the kind.
                ("async fn create_schedule(", "create_task("),
            ] {
                let body = handler_body(signature);
                let check = body.find("rule_spec_rejection(").unwrap_or_else(|| {
                    panic!(
                        "{signature} no longer validates the rule spec — a rule with \
                         no actions, or with a time window that parses as nothing, \
                         would be stored and then never fire"
                    )
                });
                let store = body
                    .find(store_call)
                    .unwrap_or_else(|| panic!("{signature} no longer calls {store_call}"));
                assert!(
                    check < store,
                    "{signature} calls {store_call} before rule_spec_rejection(), \
                     so the rule is stored whatever the check says"
                );
                // The answer must be RETURNED: a call whose result is only logged refuses nothing.
                assert!(
                    rejection_arm(signature, store_call).contains("return (status, Json(body))"),
                    "{signature} calls rule_spec_rejection() and does not return \
                     what it answers, so the 400 naming the field never reaches \
                     the caller and the rule is stored anyway"
                );
            }
        }

        #[test]
        fn the_return_window_is_the_arm_and_not_the_handler() {
            // Later `(status, Json(body))` returns would satisfy a whole-body window.
            let arm = rejection_arm("async fn create_rule(", "create_task(");
            let body = handler_body("async fn create_rule(");
            assert!(
                arm.len() < body.len() / 2,
                "the window has grown into the rest of the handler: {} of {} bytes",
                arm.len(),
                body.len()
            );
            assert!(
                !arm.contains("scheduler returned a non-rule for a rule create"),
                "the window runs past the store call it is meant to end at, so \
                 the `return` it finds may be one further down the handler"
            );
        }

        #[test]
        fn the_handlers_refuse_a_spec_the_domain_refuses() {
            let mut bad = spec();
            bad.actions.clear();
            assert!(bad.validate().is_err());

            let mut bad = spec();
            bad.condition.after = Some("half six".into());
            assert!(bad.validate().is_err());
        }
    }

    // ── image attachment limits ──────────────────────────────────

    fn attachment(bytes: usize, mime: &str) -> pond_core::models::domain::message::ImageAttachment {
        pond_core::models::domain::message::ImageAttachment {
            data: "A".repeat(bytes.div_ceil(3) * 4),
            mime_type: mime.to_string(),
        }
    }

    fn limit_status(
        images: &[pond_core::models::domain::message::ImageAttachment],
    ) -> Option<StatusCode> {
        image_limit_response(images).err().map(|(s, _)| s)
    }

    #[test]
    fn a_text_only_turn_and_a_legal_attachment_set_both_pass() {
        assert!(limit_status(&[]).is_none());
        assert!(limit_status(&[attachment(64_000, "image/jpeg")]).is_none());
    }

    #[test]
    fn too_many_images_is_413_not_400() {
        use pond_core::models::domain::image_limits::MAX_IMAGES_PER_TURN;
        let images: Vec<_> = (0..MAX_IMAGES_PER_TURN + 1)
            .map(|_| attachment(1024, "image/jpeg"))
            .collect();
        assert_eq!(limit_status(&images), Some(StatusCode::PAYLOAD_TOO_LARGE));
    }

    #[test]
    fn an_oversized_image_is_413() {
        use pond_core::models::domain::image_limits::MAX_IMAGE_BYTES;
        assert_eq!(
            limit_status(&[attachment(MAX_IMAGE_BYTES + 4096, "image/png")]),
            Some(StatusCode::PAYLOAD_TOO_LARGE)
        );
    }

    #[test]
    fn the_aggregate_budget_is_also_413() {
        use pond_core::models::domain::image_limits::MAX_IMAGE_BYTES;
        let images: Vec<_> = (0..3)
            .map(|_| attachment(MAX_IMAGE_BYTES - 1024, "image/jpeg"))
            .collect();
        assert_eq!(limit_status(&images), Some(StatusCode::PAYLOAD_TOO_LARGE));
    }

    #[test]
    fn an_unsupported_container_is_415() {
        assert_eq!(
            limit_status(&[attachment(1024, "application/pdf")]),
            Some(StatusCode::UNSUPPORTED_MEDIA_TYPE)
        );
    }

    #[test]
    fn an_empty_payload_is_400() {
        assert_eq!(
            limit_status(&[pond_core::models::domain::message::ImageAttachment {
                data: String::new(),
                mime_type: "image/png".to_string(),
            }]),
            Some(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn the_rejection_body_names_the_offending_size() {
        use pond_core::models::domain::image_limits::MAX_IMAGE_BYTES;
        let (_, Json(body)) =
            image_limit_response(&[attachment(MAX_IMAGE_BYTES + 4096, "image/png")]).unwrap_err();
        let msg = body["error"].as_str().unwrap();
        assert!(msg.contains("MB"), "expected a size in the message: {msg}");
        assert!(msg.contains("resize"), "expected advice: {msg}");
    }

    // ── attachment URL escaping ──────────────────────────────────

    #[test]
    fn an_attachment_url_cannot_smuggle_extra_path_segments() {
        assert_eq!(urlencoding_lite("abc-123_x.y~"), "abc-123_x.y~");
        assert_eq!(urlencoding_lite("../evil"), "..%2Fevil");
        assert_eq!(urlencoding_lite("a?b=c"), "a%3Fb%3Dc");
    }

    // ── Spotify failure classification ───────────────────────────

    #[test]
    fn spotify_403_is_reported_as_an_authorisation_problem() {
        let (code, message) = spotify_error_hint(StatusCode::FORBIDDEN);
        assert_eq!(code, "forbidden");
        assert!(
            message.contains("developer dashboard") && message.contains("SPOTIFY_CLIENT_ID"),
            "403 message must name both remedies, got: {message}"
        );
    }

    #[test]
    fn spotify_401_asks_the_user_to_sign_in_again() {
        let (code, message) = spotify_error_hint(StatusCode::UNAUTHORIZED);
        assert_eq!(code, "unauthorized");
        assert!(message.to_lowercase().contains("sign in"));
    }

    #[test]
    fn spotify_failures_are_distinguishable_from_each_other() {
        let codes = [
            spotify_error_hint(StatusCode::UNAUTHORIZED).0,
            spotify_error_hint(StatusCode::FORBIDDEN).0,
            spotify_error_hint(StatusCode::TOO_MANY_REQUESTS).0,
            spotify_error_hint(StatusCode::BAD_GATEWAY).0,
        ];
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(
            unique.len(),
            codes.len(),
            "each failure needs its own code — collapsing them is the bug this guards"
        );
        // 204 must never reach here: it is the one genuine "nothing playing".
        assert!(!codes.contains(&"idle"));
    }

    // ── geocode-on-save decision ─────────────────────────────────

    #[test]
    fn geocodes_when_coordinates_are_unset() {
        // Onboarding: a name saved with 0,0 and no coord keys in the patch.
        assert_eq!(
            geocode_target("Nairobi", "Nairobi", 0.0, 0.0, None, None, 0.0, 0.0),
            Some("Nairobi".to_string())
        );
    }

    #[test]
    fn geocodes_when_the_name_changed_even_if_old_coords_are_echoed() {
        // Settings sends the whole object, echoing the old coordinates with the new name.
        assert_eq!(
            geocode_target(
                "Kisumu",
                "Nairobi",
                -1.29,
                36.82,
                Some(-1.29),
                Some(36.82),
                -1.29,
                36.82
            ),
            Some("Kisumu".to_string())
        );
    }

    #[test]
    fn does_not_geocode_an_unchanged_name_with_coordinates() {
        assert_eq!(
            geocode_target(
                "Nairobi",
                "Nairobi",
                -1.29,
                36.82,
                Some(-1.29),
                Some(36.82),
                -1.29,
                36.82
            ),
            None
        );
    }

    #[test]
    fn does_not_geocode_when_the_user_edited_coordinates() {
        // Patched coordinates differ from stored, so they are an explicit edit.
        assert_eq!(
            geocode_target(
                "Nairobi",
                "Nairobi",
                40.0,
                -74.0,
                Some(40.0),
                Some(-74.0),
                -1.29,
                36.82
            ),
            None
        );
    }

    #[test]
    fn does_not_geocode_an_empty_or_whitespace_name() {
        assert_eq!(geocode_target("", "", 0.0, 0.0, None, None, 0.0, 0.0), None);
        assert_eq!(
            geocode_target("   ", "x", 0.0, 0.0, None, None, 0.0, 0.0),
            None
        );
    }

    // ── Memory-fit guard ─────────────────────────────────────────

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
        // Mirrors the desktop's DEFAULT_HEADROOM_MB so the server warning and UI badge agree.
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

    /// Captured from `pond-mcp-server`'s `print_a_real_rendered_result`; its URL is colon-heavy.
    /// The marker ends at the first `]]]`, so the producer must substitute any `]]]` in its text.
    #[test]
    fn extract_ui_hint_parses_a_real_wolfram_result() {
        let input = concat!(
            r#"[[[mcp-ui:wolfram:{"explore":[{"id":"w1","kind":"assumption","label":"a word","#,
            r#""verb":"interpret as"}],"pods":[{"text":"about 1.2 x the length of Central Park","#,
            r#""title":"Comparison"}],"primary":"4.828 km","primary_title":"Result","#,
            r#""query":"3 miles in km","source_url":"#,
            r#""https://www.wolframalpha.com/input?i=3%20miles%20in%20km"}]]]"#,
            "\n**Result**: 4.828 km",
        );
        let (clean, hint) = extract_ui_hint(input);
        assert_eq!(clean, "**Result**: 4.828 km");

        let ui = hint.expect("a real Wolfram result must produce a hint");
        // `findCardByHint` looks the card up by this string: the contract with WolframCard.tsx.
        assert_eq!(ui["card_type"], "wolfram");
        assert_eq!(ui["data"]["primary"], "4.828 km");
        // The URL's own colons must not have been mistaken for the separator.
        assert_eq!(
            ui["data"]["source_url"],
            "https://www.wolframalpha.com/input?i=3%20miles%20in%20km"
        );
        // `explore_computation` resolves these ids; without them the card's chips point nowhere.
        assert_eq!(ui["data"]["explore"][0]["id"], "w1");
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
        let input = "[[[mcp-ui:weather]]]\nSome text";
        let (clean, hint) = extract_ui_hint(input);
        assert_eq!(clean, "Some text");
        assert!(hint.is_none(), "missing colon should not produce a hint");
    }

    // ── The stream translator ────────────────────────────────────────────
    // `TurnAccumulator::absorb` alone turns `AgentStreamEvent`s into SSE frames, for both routes.
    // Compare frames whole: a `tool`/`id` swap keeps the `type` but matches no card.

    use pond_core::models::ports::agent::AgentStreamEvent;
    use pond_core::models::ports::provider::UsageStats;
    // Not re-exported by `models::ports::agent`, which exports only what its signatures need.
    use pond_core::shared::domain::agent::SubagentStatus;

    /// An accumulator configured the way `/agent/chat/stream` configures one.
    fn accumulator() -> TurnAccumulator {
        TurnAccumulator::new(crate::thought_filter::ThoughtFilter::new())
    }

    fn frame_of(step: StreamStep) -> Value {
        match step {
            StreamStep::Frame(data) => {
                serde_json::from_str(&data).expect("a frame is JSON the browser can parse")
            }
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    /// Long enough to clear the filter's `<|channel>` lookahead; a short one would emit nothing.
    const SENTENCE: &str = "The kettle is on and the hallway lights are off, as you asked earlier.";

    #[test]
    fn a_visible_token_becomes_a_text_frame_and_joins_the_answer() {
        let mut turn = accumulator();
        let frame = frame_of(turn.absorb(AgentStreamEvent::Text {
            content: SENTENCE.to_string(),
        }));

        assert_eq!(frame["type"], "text");
        let shown = frame["content"].as_str().expect("content is a string");
        assert!(
            !shown.is_empty() && SENTENCE.starts_with(shown),
            "the frame must carry what the filter released, got {shown:?}"
        );
        // The desktop reads `token`, the hub `content`; both must be set.
        assert_eq!(frame["token"], frame["content"]);
        assert_eq!(
            turn.full_text, shown,
            "the persisted answer must be exactly what was shown"
        );
    }

    #[test]
    fn text_the_filter_is_still_holding_emits_nothing() {
        let mut turn = accumulator();
        let step = turn.absorb(AgentStreamEvent::Text {
            content: "<|channel>thought I should check the".to_string(),
        });

        assert_eq!(
            step,
            StreamStep::Nothing,
            "reasoning markup must not reach the browser as answer text"
        );
        assert!(turn.full_text.is_empty());
        assert!(
            turn.ttft.is_none(),
            "time-to-first-token is about the answer, not about a swallowed chunk"
        );
    }

    #[test]
    fn the_first_visible_token_stamps_ttft_and_a_later_one_does_not_move_it() {
        let mut turn = accumulator();
        turn.absorb(AgentStreamEvent::Text {
            content: SENTENCE.to_string(),
        });
        let first = turn.ttft.expect("the first visible token stamps TTFT");
        turn.absorb(AgentStreamEvent::Text {
            content: SENTENCE.to_string(),
        });
        assert_eq!(
            turn.ttft,
            Some(first),
            "TTFT is the FIRST token; re-stamping it reports the last one instead"
        );
    }

    /// `TurnMetrics` timing is gathered only here, and its consumers have no test in this crate.
    #[test]
    fn a_tool_call_and_its_result_time_the_turn() {
        let mut turn = accumulator();
        let call = frame_of(turn.absorb(AgentStreamEvent::ToolCall {
            tool: "giap-weather__get_weather".to_string(),
            id: "call-1".to_string(),
            input: Some(json!({"location": "Nairobi"})),
        }));
        assert_eq!(
            call,
            json!({
                "type": "tool_call",
                "tool": "giap-weather__get_weather",
                "id": "call-1",
                "input": {"location": "Nairobi"},
            }),
            "the tool_call frame is what the desktop opens a tool card from: `tool` \
             names the card and `id` is the handle its result is matched against \
             later. Compared whole, because a frame with two of those transposed \
             still has the right `type`"
        );
        assert_eq!(
            turn.last_tool_name.as_deref(),
            Some("giap-weather__get_weather")
        );
        assert!(
            turn.tool_call_start.is_some(),
            "without a start instant the result below has nothing to subtract from"
        );

        let result = frame_of(turn.absorb(AgentStreamEvent::ToolResult {
            tool: "giap-weather__get_weather".to_string(),
            id: "call-1".to_string(),
            content: "24C and clear".to_string(),
        }));
        assert_eq!(
            result,
            json!({
                "type": "tool_result",
                "tool": "giap-weather__get_weather",
                "id": "call-1",
                "content": "24C and clear",
            }),
            "`tool_result_frame` is called from four sites and its two identifying \
             arguments are adjacent `&str`s, so transposing them compiles. The \
             desktop then matches the result to no card at all: the spinner clears \
             and the card stays empty"
        );
        assert!(
            turn.last_tool_latency_ms.is_some(),
            "the result must close the timing the call opened, or TurnMetrics \
             reports a tool with no latency"
        );
        assert!(
            turn.tool_call_start.is_none(),
            "the start instant is consumed, so a second result cannot re-time \
             the first call"
        );

        assert_eq!(turn.tool_results.len(), 1, "the turn persists one result");
        let persisted: Value = serde_json::from_str(&turn.tool_results[0]).unwrap();
        assert_eq!(
            persisted,
            json!({
                "tool_call_id": "call-1",
                "tool": "giap-weather__get_weather",
                "content": "24C and clear",
                // From the ToolCall event: ToolResult doesn't repeat the arguments.
                "arguments": "{\"location\":\"Nairobi\"}",
            }),
            "the persisted row is replayed into the next prompt as the model's own \
             tool history; a row whose id and name are transposed teaches the model \
             it called a tool named `call-1`"
        );
    }

    #[test]
    fn a_ui_hint_rides_the_frame_and_stays_out_of_the_transcript() {
        let mut turn = accumulator();
        let frame = frame_of(turn.absorb(AgentStreamEvent::ToolResult {
            tool: "giap-weather__get_weather".to_string(),
            id: "call-1".to_string(),
            content: "[[[mcp-ui:weather:{\"temp\":24}]]]\n24C and clear".to_string(),
        }));

        assert_eq!(
            frame,
            json!({
                "type": "tool_result",
                "tool": "giap-weather__get_weather",
                "id": "call-1",
                "content": "24C and clear",
                "ui": {"card_type": "weather", "data": {"temp": 24}},
            }),
            "a hinted result is still a `tool_result` frame and still has to reach \
             the card its call opened -- `ui` is what that card renders as, not a \
             substitute for the two keys that find it"
        );

        let persisted: Value = serde_json::from_str(&turn.tool_results[0]).unwrap();
        assert_eq!(
            persisted["content"], "24C and clear",
            "the marker must not be persisted -- it would be replayed into the \
             next prompt as literal text"
        );
    }

    /// The handler's `ChatService` owns the `persist_thinking` gate; the translator never records.
    #[test]
    fn a_reasoning_passage_comes_back_whole_for_the_persistence_owner() {
        let mut turn = accumulator();
        let step = turn.absorb(AgentStreamEvent::Thinking {
            content: "  the kettle is a device, so I should look it up  ".to_string(),
        });

        let StreamStep::Reasoning { frame, block } = step else {
            panic!("thinking must come back as Reasoning, not as a bare frame");
        };
        assert_eq!(
            block, "  the kettle is a device, so I should look it up  ",
            "the handler needs the passage as the model wrote it, not the frame"
        );
        let frame: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(frame["type"], "thinking");
        assert_eq!(frame["content"], block);
        assert!(
            turn.full_text.is_empty(),
            "reasoning is never part of the answer that gets persisted"
        );
    }

    /// The routes disagree on `done`; as a frame it would reach clients early or twice.
    #[test]
    fn the_engines_done_is_numbers_for_the_route_to_report_not_a_frame() {
        let mut turn = accumulator();
        let step = turn.absorb(AgentStreamEvent::Done {
            session_id: "s-1".to_string(),
            model_role: "chat".to_string(),
            usage: Some(UsageStats {
                prompt_tokens: 1_200,
                completion_tokens: 96,
                reasoning_tokens: Some(40),
            }),
            stats: None,
        });

        let StreamStep::TurnComplete { usage, stats } = step else {
            panic!("Done must come back as TurnComplete, got {step:?}");
        };
        let usage = usage.expect("the engine's usage is carried through, not dropped");
        assert_eq!(usage.prompt_tokens, 1_200);
        assert_eq!(usage.completion_tokens, 96);
        assert_eq!(
            usage.reasoning_tokens,
            Some(40),
            "PAI-5 P2 reports reasoning ALONGSIDE the completion count"
        );
        assert!(stats.is_none());
    }

    /// Distinct `score`/`rounds` values (4, 2) so transposing them in `review_revision` fails.
    #[test]
    fn the_status_shaped_variants_keep_the_type_the_client_switches_on() {
        let mut turn = accumulator();
        for (step, expected) in [
            (
                turn.absorb(AgentStreamEvent::Status {
                    content: "Agent working".to_string(),
                }),
                json!({"type": "status", "content": "Agent working"}),
            ),
            (
                turn.absorb(AgentStreamEvent::ReviewStatus {
                    content: "Reviewing answer".to_string(),
                }),
                json!({"type": "review_status", "content": "Reviewing answer"}),
            ),
            (
                turn.absorb(AgentStreamEvent::ReviewRevision {
                    content: "A better answer".to_string(),
                    score: 4,
                    rounds: 2,
                }),
                json!({
                    "type": "review_revision",
                    "content": "A better answer",
                    "score": 4,
                    "rounds": 2,
                }),
            ),
            (
                turn.absorb(AgentStreamEvent::TurnLimitReached { max_turns: 25 }),
                json!({"type": "turn_limit_reached", "max_turns": 25}),
            ),
        ] {
            assert_eq!(
                frame_of(step),
                expected,
                "the frame the client receives must be this frame whole, not just \
                 something with the right `type`"
            );
        }

        // No `type` on purpose: clients detect errors by the `error` key.
        let err = frame_of(turn.absorb(AgentStreamEvent::Error {
            content: "the model went away".to_string(),
        }));
        assert_eq!(err["error"], "the model went away");
        assert!(err.get("type").is_none());
    }

    /// Subagent progress reaches the client only, never the persisted `full_text`/`tool_results`.
    #[test]
    fn absorb_progress_leaves_the_turn_untouched() {
        let mut turn = accumulator();
        // A real prior call, so "untouched" can't pass because nothing was ever written.
        turn.absorb(AgentStreamEvent::ToolCall {
            id: "call-1".to_string(),
            tool: "giap-orchestrator__delegate".to_string(),
            input: None,
        });

        for (status, detail) in [
            (SubagentStatus::Queued, None),
            (SubagentStatus::Running, None),
            (
                SubagentStatus::Tool,
                Some("giap-memory__recall_memories".to_string()),
            ),
            (SubagentStatus::Completed, None),
        ] {
            let frame = frame_of(turn.absorb(AgentStreamEvent::SubagentProgress {
                task_id: "task-1".to_string(),
                role: "researcher".to_string(),
                status,
                detail: detail.clone(),
            }));
            assert_eq!(
                frame,
                json!({
                    "type": "subagent_progress",
                    "task_id": "task-1",
                    "role": "researcher",
                    "status": status.as_str(),
                    "detail": detail,
                }),
                "the frame the client receives must be this frame whole"
            );
        }

        assert!(
            turn.full_text.is_empty(),
            "a subagent's progress joined the parent's answer text, which is \
             what gets persisted as the assistant message -- PAI-6 invariant 4 \
             says only the delegation's RESULT may do that"
        );
        assert!(
            turn.tool_results.is_empty(),
            "a subagent's progress was persisted as one of the parent turn's \
             tool results"
        );
        assert_eq!(
            turn.last_tool_name.as_deref(),
            Some("giap-orchestrator__delegate"),
            "the child's tool overwrote the parent's own last tool, so \
             TurnMetrics now reports a tool this turn never called"
        );
        assert!(
            turn.last_tool_latency_ms.is_none(),
            "a progress frame closed the parent's open tool-call timing, so the \
             delegate call's latency is now the gap before the child's first \
             tool instead of the whole delegation"
        );
    }

    // ── Profile particulars are Owner-scoped ──────────────────────────────

    /// Keys spelled out literally, so a rename on either side fails a test.
    fn full_prefs() -> std::collections::HashMap<String, String> {
        [
            ("preferred_name", "Cap"),
            ("birthday", "1990-04-02"),
            ("language", "sw"),
            ("accessibility_atypical_speech", "true"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn an_owner_scoped_turn_states_the_members_particulars() {
        let ctx = particulars_for(true, &full_prefs());
        assert_eq!(ctx.preferred_name.as_deref(), Some("Cap"));
        assert_eq!(ctx.birthday.as_deref(), Some("1990-04-02"));
        assert_eq!(ctx.language.as_deref(), Some("sw"));
        assert!(ctx.atypical_speech);
    }

    /// `Household` uses `primary_profile_id`, which may not be the person speaking.
    #[test]
    fn a_household_turn_states_no_ones_particulars() {
        let ctx = particulars_for(false, &full_prefs());
        assert_eq!(
            ctx.preferred_name, None,
            "an unattributed turn stated the primary member's preferred name"
        );
        assert_eq!(
            ctx.birthday, None,
            "an unattributed turn stated the primary member's birthday -- a fact about one \
             person, asserted while the pond does not know who is speaking"
        );
        assert_eq!(
            ctx.language, None,
            "an unattributed turn adopted the primary member's language, which would answer \
             everybody else in it too"
        );
    }

    /// Asserted separately so narrowing this deliberate exception is a visible choice.
    #[test]
    fn the_speech_accommodation_survives_an_unattributed_turn() {
        assert!(
            particulars_for(false, &full_prefs()).atypical_speech,
            "the speech accommodation was dropped for unattributed turns; it is not a \
             disclosure and dropping it makes the pond less patient with the household it was \
             configured for"
        );
    }

    /// Absent, not `Some("")`: the prompt builder would render "The user's birthday is ."
    #[test]
    fn a_profile_with_no_preferences_yields_nothing_to_state() {
        let ctx = particulars_for(true, &std::collections::HashMap::new());
        assert_eq!(ctx.preferred_name, None);
        assert_eq!(ctx.birthday, None);
        assert_eq!(ctx.language, None);
        assert!(!ctx.atypical_speech);
    }

    /// The desktop's `preferredName`/`atypicalSpeech` spellings, if stored, are silently ignored.
    #[test]
    fn camel_case_keys_are_not_read_and_that_is_the_point() {
        let camel: std::collections::HashMap<String, String> =
            [("preferredName", "Cap"), ("atypicalSpeech", "true")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        let ctx = particulars_for(true, &camel);
        assert_eq!(
            ctx.preferred_name, None,
            "camelCase keys are now read, so the two spellings have silently become \
             equivalent and the writer contract is no longer pinned by anything"
        );
        assert!(!ctx.atypical_speech);
    }

    /// A recognised name mapped to a missing group narrows the recipe to zero tools, silently.
    #[test]
    fn every_recipe_extension_maps_to_a_tool_group_that_exists() {
        // Literal, not derived: the point is to exercise the match arms.
        const RECIPE_EXTENSION_NAMES: &[&str] = &[
            "weather",
            "schedule",
            "scheduler",
            "memory",
            "device",
            "developer",
            "matter",
            "home",
        ];

        let mut mapped = 0usize;
        for name in RECIPE_EXTENSION_NAMES {
            let Some(group) = recipe_extension_to_tool_group(name) else {
                panic!(
                    "'{name}' is in this test's list but maps to nothing -- either the arm was                      removed on purpose, in which case drop it from the list, or by accident"
                );
            };
            mapped += 1;
            assert!(
                pond_core::mcp::domain::tool_group::is_catalog_extension(group),
                "recipe extension '{name}' maps to '{group}', which is not in TOOL_GROUPS. A                  recipe declaring `extensions: [{name}]` will narrow to that group, match no                  tool, and run with none."
            );
        }

        // Vacuity control: an emptied list would pass having asserted nothing.
        assert_eq!(
            mapped,
            RECIPE_EXTENSION_NAMES.len(),
            "the mapping list is not exercising the match arms"
        );
    }

    /// The on-disk UI fallback is auth-exempt, so it must never read outside `static_dir`.
    mod static_dir_containment {
        use super::super::serve_from_disk;
        use axum::http::StatusCode;

        #[tokio::test]
        async fn a_path_that_climbs_out_of_the_static_dir_is_not_served() {
            let root = tempfile::tempdir().unwrap();
            let dist = root.path().join("dist");
            std::fs::create_dir_all(dist.join("assets")).unwrap();
            std::fs::write(dist.join("index.html"), "app").unwrap();
            std::fs::write(root.path().join("secret.txt"), "secret").unwrap();

            for rel in [
                "../secret.txt",
                "assets/../../secret.txt",
                "./../secret.txt",
            ] {
                let resp = serve_from_disk(&dist, rel).await;
                assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{rel} was served");
            }
        }

        #[tokio::test]
        async fn files_inside_the_static_dir_are_still_served() {
            let dist = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dist.path().join("assets")).unwrap();
            std::fs::write(dist.path().join("index.html"), "app").unwrap();
            std::fs::write(dist.path().join("assets/app.js"), "js").unwrap();

            for rel in ["index.html", "assets/app.js", "./assets/app.js", "settings"] {
                let resp = serve_from_disk(dist.path(), rel).await;
                assert_eq!(resp.status(), StatusCode::OK, "{rel} was not served");
            }
        }
    }
}
