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
    //
    // "Public" here means "not behind `require_onboarding_complete`". Whether a
    // route answers a caller with no bearer token is a SEPARATE question,
    // decided by `middleware::PUBLIC_ROUTES` -- and since PAI-2 P7 the answer
    // depends on the pond's state: the wizard's writes (`PUT /settings`,
    // `POST /profiles`, `PATCH /profiles/{id}`, `POST /onboard/*`,
    // `/voice/calibrate`) stop answering anonymous callers once onboarding
    // completes. Adding a route here means classifying it there; four tests
    // fail the build otherwise.
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
        // Prefix warm-up status: is the pond ready for a first message yet.
        // Read-only, and open for the same window /settings write is open --
        // the wizard shows the banner before onboarding-gated auth exists, and
        // the client sends a token once it has one. See PUBLIC_ROUTES.
        .route("/warmup", get(get_warmup))
        // ── Time and place ────────────────────────────────────────────────
        // One catalogue and one detection, so the three screens that ask
        // "where is this pond" stop each answering it differently. Public
        // because the wizard sets location up before any device has paired;
        // see PUBLIC_ROUTES for what each of the two exposes.
        .route("/time/zones", get(list_time_zones))
        .route("/location/detect", post(detect_location))
        // TTS synthesis is public so the onboarding voice-preview can play a
        // sample before onboarding completes. Text→audio via local Piper is not
        // privileged and leaks no user data.
        .route("/tts", post(tts_synthesise))
        // Transcription proxy (public — local test tool)
        .route("/transcribe", post(transcribe))
        // Wake-word phrase calibration (public — used during onboarding WakeWord step)
        .route("/voice/tts/apply", post(apply_tts_settings))
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
        // Phase F1: raise the body ceiling for the ONE route that carries image
        // attachments. Axum's 2 MiB default is smaller than a legal attachment
        // set, so without this the framework rejects a 3 MB photo with "length
        // limit exceeded" and `image_limit_response` — which knows how to
        // explain the problem — never runs. Scoped to this route rather than the
        // router: no other endpoint has any business accepting 12 MiB.
        // Following a turn already in flight, and stopping one on purpose. Both
        // sit on the protected router, so `require_auth` and the onboarding gate
        // apply exactly as they do to the turn itself.
        .route("/chat/runs/{run_id}/events", get(reattach_run_events))
        .route("/chat/runs/{run_id}/cancel", post(cancel_run))
        // Discovery by session, because a client that restarted knows its
        // session id and nothing else. `delete` for "stop it", matching how the
        // other session-scoped routes here spell that.
        .route(
            "/sessions/{session_id}/active-run",
            get(session_active_run).delete(cancel_session_run),
        )
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
        // PAI-4 P7 — the manual axis. The time axis (P4) and the pressure axis
        // (P6) both decide for the user; this is the one a person decides.
        .route("/sessions/{session_id}/compact", post(compact_session))
        .route("/sessions/retitle", post(retitle_sessions))
        .route("/sessions/{session_id}/retitle", post(retitle_session))
        // Delete a message and every later message in the same session — the
        // "edit"/"refresh" primitive: the client truncates from a user
        // message, then resubmits (same or edited text) as a normal new turn.
        .route(
            "/sessions/{session_id}/messages/{message_id}",
            delete(delete_messages_from_handler),
        )
        // Like/dislike training-feedback on one message.
        .route(
            "/sessions/{session_id}/messages/{message_id}/feedback",
            put(set_message_feedback_handler),
        )
        // Phase F2: raw bytes for one persisted image attachment.
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
        // Push-notification token register/unregister for a paired device (#95).
        .route(
            "/devices/{id}/push-token",
            post(register_push_token).delete(delete_push_token),
        )
        // Private mesh (#132) — trust-circle peers + self invite.
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
        // Activity query API (#114) — read the unified event log.
        // DELETE clears activity on demand (#117, "clear my activity").
        .route("/activity", get(get_activity).delete(clear_activity))
        .route("/activity/summary", get(activity_summary))
        // PAI-2 P8a — the would-deny telemetry the enforce flip is waiting on.
        // Protected by registration here and by its absence from PUBLIC_ROUTES.
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
        // ── Sensor rules (PAI-7 P8) ────────────────────────────────────────
        // A rule is a `SensorTrigger` schedule. Until now the only way to make
        // one over HTTP was to POST that kind to `/schedules` along with a cron
        // expression the scheduler never reads, which is why the MCP tools were
        // the real interface. These handlers name the thing, validate the spec
        // the schedule route accepts unchecked, and refuse to reach a cron
        // schedule by id.
        .route("/rules", get(list_rules).post(create_rule))
        .route(
            "/rules/{id}",
            get(get_rule).put(update_rule).delete(delete_rule),
        )
        .route("/rules/{id}/pause", post(pause_rule))
        .route("/rules/{id}/resume", post(resume_rule))
        // ── Proactive proposals (PAI-7 P3b) ────────────────────────────────
        // The surface PAI-7 P3a's domain has been waiting for. Protected, and
        // deliberately absent from `middleware::PUBLIC_ROUTES`: a proposal is
        // addressed to one member and an anonymous caller is not one.
        .route("/proposals", get(list_proposals))
        .route(
            "/context/sources",
            get(list_context_sources).post(connect_context_source),
        )
        .route("/context/sources/{id}", delete(disconnect_context_source))
        .route("/context/sync", post(sync_context_sources))
        .route("/context/items", get(list_context_items))
        // ── The index's own health, and the way to repair it ───────────────
        // Protected, and deliberately absent from `middleware::PUBLIC_ROUTES`:
        // these counts say how much of a household's memory exists and how
        // much of it retrieval can currently reach, which is a description of
        // that household, and the rebuild throws work at the machine.
        .route("/context/index/health", get(context_index_health))
        .route("/context/index/rebuild", post(rebuild_context_index))
        .route("/proposals/{id}/decide", post(decide_proposal))
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
            get(get_session_user_handler)
                .put(set_session_user_handler)
                .delete(clear_session_user_handler),
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
            // 21, the length of the marker. With 20 the windows could never
            // equal it, `any` was always false, and this function always
            // returned true — so the "UI not embedded" startup warning could
            // not fire and the --static-dir fallback it guards was unreachable.
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
    peer: Result<
        axum::extract::ConnectInfo<std::net::SocketAddr>,
        axum::extract::rejection::ExtensionRejection,
    >,
    body: Result<Json<HandshakeRequest>, JsonRejection>,
) -> Result<Json<HandshakeResponse>, (axum::http::StatusCode, Json<Value>)> {
    crate::network::require_lan(peer.ok())?;
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
    peer: Result<
        axum::extract::ConnectInfo<std::net::SocketAddr>,
        axum::extract::rejection::ExtensionRejection,
    >,
    body: Result<Json<InitRequest>, JsonRejection>,
) -> Result<Json<ChallengeResponse>, (StatusCode, Json<Value>)> {
    crate::network::require_lan(peer.ok())?;
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

/// Record the result of a pairing attempt, in the log and in the event log.
///
/// The event goes to the unified event log (category `Auth`, `Sensitive` —
/// surfaceable by the audit tools, never the payload itself) and a security
/// notification goes to connected devices (#164 follow-up). Both are
/// best-effort: they must never change the handshake response.
///
/// `reason` is the handshake's own `rejection_reason` on a failure: a closed set
/// of short codes (`invalid_mac`, `challenge_expired`, `unknown_challenge`, and
/// so on). It is safe to log -- none of them carries the pairing code or the MAC,
/// and neither is logged anywhere else either.
///
/// Recording it matters because a failed attempt used to leave nothing behind at
/// all. `Handshake::reject` builds a response and returns; success wrote one INFO
/// line and failure wrote nothing, so an operator working through several tries
/// could not tell which had failed, let alone why. The phone alert fires, but it
/// is debounced to one per ten minutes and says only that something failed.
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

    // Both edges, so tailing the log shows every attempt and its outcome.
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
        // Without this the event log records that a pairing failed but not what
        // went wrong, which is the one thing worth going back for.
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
    peer: Result<
        axum::extract::ConnectInfo<std::net::SocketAddr>,
        axum::extract::rejection::ExtensionRejection,
    >,
    body: Result<Json<VerifyRequest>, JsonRejection>,
) -> Result<Json<HandshakeResponse>, (StatusCode, Json<Value>)> {
    let peer = peer.ok();
    crate::network::require_lan(peer)?;
    let peer = peer.expect("LAN guard requires a connection address").0;
    // Rate-limit verify attempts per source IP (applies to loopback too — this
    // endpoint is security-sensitive regardless of origin).
    if let Err(remaining) = verify_limiter()
        .check_rate_limit_detailed(&peer.ip().to_string())
        .await
    {
        // Tell the client how long to wait rather than leaving it to guess.
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
        Some(pc) => Ok(Json(json!({
            "code": pc.code,
            "expires_at": pc.expires_at,
            // PAI-1 P9: whose device this code will make. `null` is an
            // unattributed code, which is every code issued before the field
            // existed and every code issued with no body.
            "profile_id": pc.profile_id,
        }))),
        None => Ok(Json(json!({"code": null}))),
    }
}

/// The optional body of `POST /handshake/pairing-code`.
///
/// PAI-1 P9's HTTP half. **The member is captured here, at issuance, and never
/// from the pairing request.** `IdentificationSource::PairedDevice` outranks
/// both face and explicit identification, so a `profile_id` the pairing client
/// supplied would outrank every proof this pond can actually make — the same
/// shape as the hole PAI-1 P4 closed on `PUT /sessions/{id}/user`. What makes
/// issuance trustworthy instead is the loopback check in the handler: the
/// answer comes from somebody standing at the pond.
///
/// `deny_unknown_fields` because the failure this refuses is silent otherwise:
/// an operator who posts `profileId` would be told "issued" and handed a code
/// that belongs to nobody, and an unattributed device is invisible until the
/// day somebody wonders why their phone gets no proposals.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct IssuePairingCodeRequest {
    /// The household member the device pairing with this code will belong to.
    ///
    /// Omitted or `null` no longer means "unattributed" on its own: a
    /// one-member household defaults to that member, per
    /// [`member_attribution::owner_for_new_code`]. Set `unattributed` to ask
    /// for a code that binds to nobody.
    #[serde(default)]
    profile_id: Option<String>,
    /// Ask for a code that binds the device to nobody, even in a household
    /// where a member could be inferred.
    ///
    /// This is how a guest's phone is paired without becoming a member's. It is
    /// the reason the sole-member default is safe to have at all.
    #[serde(default)]
    unattributed: bool,
}

/// Issue a **fresh** single-use pairing code. **Loopback-only** — this is the
/// "pair a new device" action the operator triggers from the host (CLI/desktop
/// dashboard) to pair an additional phone after the startup code is consumed.
///
/// Optionally binds the code to a household member; see
/// [`IssuePairingCodeRequest`] for why that binding happens here and nowhere
/// else.
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
    // The body is optional and read as raw bytes rather than through `Json`,
    // because this route has been POSTed with no body and no content type since
    // #93 and both the CLI and the desktop dashboard still do that. An empty
    // body means an unattributed code, exactly as before.
    //
    // A body that is PRESENT and unreadable is refused rather than defaulted.
    // Defaulting would narrow — an unattributed code grants nothing — but it
    // would also tell an operator who meant to bind this code to Liz that the
    // code was issued, and the device would silently be nobody's.
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
    // The member ids the default is allowed to consider. A failed read narrows
    // to an unattributed code rather than assuming the household is one person,
    // exactly as `resolve_turn_scope` narrows on the same failure: pairing is
    // recoverable, and a wrong attribution rides on every turn the device sends.
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
            // Logged the same way either way, so the real cause is in the log
            // and never in the response. The status differs because the cause
            // does: the adapter leans on migration 0043's foreign key to refuse
            // a code for a member who does not exist, and answering that with a
            // 500 tells the operator the pond is broken when the member id is.
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
    /// Restricts this turn to only these tool-group prefixes. Set internally
    /// by `run_recipe` from a recipe's `extensions:`; ordinary chat clients
    /// have no reason to set it, but nothing stops one that wants to narrow
    /// its own turn.
    #[serde(default)]
    tool_group_allowlist: Option<Vec<String>>,
    /// Detach this turn from the response body: it keeps running when the
    /// client disconnects, and can be re-attached through
    /// `/chat/runs/{run_id}/events`.
    ///
    /// **Defaults to false, and that default is load-bearing rather than
    /// cautious.** `WebVoiceBackend` fires a speculative `/chat/stream` the
    /// moment trailing silence begins — before the pause is confirmed — and
    /// aborts it when speech resumes. A detached speculative turn would run to
    /// completion and persist a question-and-answer pair for a half-sentence
    /// nobody finished saying. The voice path stays ephemeral until it cancels
    /// through the endpoint instead of by dropping its socket.
    #[serde(default)]
    resumable: bool,
}

/// Send a message and get a response.
///
/// Creates a new session if `session_id` is not provided.
/// Persists both user and assistant messages to session storage.
async fn chat(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    body: Result<Json<ChatRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // PAI-1 P9. Taken from the request's principal before the body is even
    // parsed, so there is no point at which a field of that body could be
    // mistaken for it.
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
    // Resolved exactly as /chat/stream does. Without this the two endpoints
    // disagreed about the same speaker: the stream gave Guest, this gave the
    // whole household.
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

/// Bring the live speech engine in line with the saved settings.
///
/// Everything the voice picker changes used to take effect only on the next
/// start, because the engine was built once at boot. For a device that lives on
/// a shelf, that made choosing a voice look like it did nothing.
///
/// Anything missing is fetched first, so selecting a voice the household does
/// not have yet is a download rather than an error telling them to install it
/// somewhere else. Fields omitted from the body fall back to what is saved,
/// which lets the picker send only what changed.
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

/// Synthesise speech server-side and return WAV audio bytes.
///
/// Priority order:
/// 1. In-process `VoiceOutput` (`AppState.tts` — the default build's
///    `KokoroOutput`, no subprocess or extra port involved).
/// 2. Legacy Piper HTTP server (if running — see `AppState.piper_http_port`),
///    for `--features legacy-subprocess` builds.
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
                // Engine ran but produced no audio (e.g. silent/empty synthesis) —
                // fall through to the legacy HTTP path rather than erroring.
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
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    body: Result<Json<ChatRequest>, JsonRejection>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)>
{
    // PAI-1 P9. Read from the principal before the body is parsed; `ChatRequest`
    // has no device field and must never grow one.
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

    // Phase F1: reject an oversized attachment set BEFORE the stream opens, so
    // the client gets a real HTTP status it can show rather than an SSE error
    // event mid-conversation. Nothing has been decoded at this point.
    image_limit_response(&req.images)?;

    if !req.resumable {
        // Today's contract, unchanged: the turn ends when the last reader does.
        // Not registered either — an ephemeral run has nothing to discover or
        // reattach to, and registering it would let ordinary chat consume the
        // detached-run cap.
        let run = new_run(&req, &device, crate::runs::RunPolicy::Ephemeral);
        return Ok(spawn_run(state, permit, run, None, req, device));
    }

    // A detached run needs its own permit, taken here rather than inside the
    // stream for the same reason `image_limit_response` is: a client can show a
    // 503 and can do nothing sensible with an SSE error mid-conversation. It is
    // NOT `sse_semaphore` — that one counts clients reading, and a detached run
    // outlives its reader, so sharing the pool would let a handful of abandoned
    // runs starve interactive chat.
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

    // First frame of a resumable run, so a client that has to reconnect knows
    // what to reconnect TO. The epoch rides along because a run id minted under
    // a different one names a run that died with the last process.
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

/// Map an image-limit violation onto an HTTP status, or pass a legal set through.
///
/// 413 for anything about size or count, 415 for an unsupported container,
/// 400 for a structurally broken payload. The message is the domain error's own
/// `Display`, which carries the offending numbers so the UI can be specific.
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

/// What one `AgentStreamEvent` means to a chat SSE stream.
///
/// PAI-5 P7. Both stream handlers cover the same variants and emit the same
/// frame JSON, and each used to carry its own copy of the match. Two exhaustive
/// matches over an enum that is still growing is a standing promise to write
/// every new variant twice — and they had already drifted, which is how
/// `/agent/chat/stream` came to persist turns it never extracted memory from.
///
/// The two outcomes that are NOT simply a frame are the two places the routes
/// legitimately differ, so they come back as data rather than being resolved
/// here:
///
/// * [`StreamStep::Reasoning`] — the frame is identical, but the passage has to
///   reach the handler's OWN `ChatService`, which holds the `persist_thinking`
///   gate. The translator is handed no service and therefore cannot record
///   against the wrong one, and each handler keeps a visible `record_thinking`
///   call rather than inheriting one it cannot see.
/// * [`StreamStep::TurnComplete`] — `/chat/stream` yields a `turn_stats` frame
///   here and its `done` frame much later, after persistence, telemetry and the
///   context-pressure check; `/agent/chat/stream` closes on the spot. That is a
///   routing decision, not a frame shape.
#[derive(Debug, PartialEq)]
enum StreamStep {
    /// Emit nothing. The thought filter swallowed the whole chunk — it is
    /// inside a reasoning block, or holding back a possible partial tag.
    Nothing,
    /// One SSE frame, ready to yield verbatim.
    Frame(String),
    /// A reasoning passage: the frame to show, and the same text for the
    /// handler to offer its persistence owner.
    Reasoning { frame: String, block: String },
    /// The engine finished the turn, with whatever numbers it reported.
    TurnComplete {
        usage: Option<pond_core::models::ports::provider::UsageStats>,
        stats: Option<pond_core::shared::domain::turn_stats::TurnStats>,
    },
}

/// The one shape of a `tool_result` SSE frame.
///
/// Four sites build this — the `ToolResult` arm and the Harmony-envelope
/// fallback in each handler — and the optional `ui` key is what the MCP-UI
/// renderer keys off. A site that forgot it renders a card as raw text.
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

/// The one shape of a `turn_stats` SSE frame.
///
/// PAI-5 P2's display half, and the reason it took a second phase: the number
/// this adds — `reasoning_tokens` — has been on `TurnStats` and in
/// `session_messages` since 2026-08-06, but this frame is built by hand from a
/// struct, so widening the struct did not widen the frame, and `routes.rs` was
/// held by other work at the time. The count reached
/// `AgentStreamEvent::Done` and stopped there.
///
/// Two properties of that field the JSON has to preserve:
///
/// * It is reported **alongside** `completion_tokens`, never deducted from it.
///   The provider's output count most likely already includes the reasoning
///   decode, nobody has measured which way for the models GIAP pins, and
///   subtracting a GIAP estimate from an engine-reported number corrupts the
///   one that was actually measured.
/// * `null` is not `0`. "Nobody counted" and "counted, and this turn thought
///   nothing" are different facts — it is why migration 0039 has no
///   `DEFAULT 0` — and PAI-5 P5 sizes `output_reserve_tokens` from the second.
///   Serialising `None` as `null` keeps that distinction on the wire; a
///   `unwrap_or(0)` here would erase it for every provider that reports no
///   reasoning at all.
///
/// One function rather than a `json!` per handler for the same reason
/// [`tool_result_frame`] is one: both stream routes emit this frame, and a
/// field added to one copy is a client that renders it on `/chat/stream` and
/// not on `/agent/chat/stream`.
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
        // The other half of what thinking cost. `inference_count` already says
        // how many times the engine ran, but it cannot distinguish a turn that
        // ran twice because it called a tool from a turn that ran twice because
        // it thought, said nothing, and had to be steered back. Only this field
        // separates them, and 0 is the honest value for an ordinary turn — this
        // one is a count, not an Option, because every turn that reaches here
        // was observed.
        "reengagements": s.reengagements,
    })
    .to_string()
}

/// The per-turn state a chat SSE handler accumulates while the engine streams.
///
/// The three fields at the top are what both handlers need in order to persist
/// the turn; the four below them are the per-tool and first-token timing that
/// `/chat/stream` reports as `TurnMetrics` (PAI-3 and PAI-4 read it). They are
/// gathered on both routes because a recorder that only runs for one caller is
/// a behaviour flag wearing a struct, and the cost is one `Instant` per tool
/// call.
struct TurnAccumulator {
    /// Strips Harmony `<|channel>thought … <channel|>` and `<think>…</think>`
    /// out of the visible token stream, and captures what it strips.
    thought: crate::thought_filter::ThoughtFilter,
    /// The assistant's visible answer, as persisted.
    full_text: String,
    /// One JSON row per tool result, as persisted.
    tool_results: Vec<String>,
    /// Tool-call id -> the arguments the model produced, kept from the
    /// `ToolCall` event so the result blob can carry them to persistence. The
    /// `ToolResult` event does not repeat them, and without them a stored
    /// tool-call record names a call nobody can reproduce.
    tool_call_inputs: std::collections::HashMap<String, String>,
    /// When the first visible token arrived, as a fallback for an engine that
    /// reports no TTFT of its own.
    ttft: Option<std::time::Instant>,
    tool_call_start: Option<std::time::Instant>,
    /// The most recent tool call's name and wall-clock latency. When a turn
    /// invokes several tools only the last survives — `TurnMetrics` has one
    /// slot.
    last_tool_name: Option<String>,
    last_tool_latency_ms: Option<u64>,
}

impl TurnAccumulator {
    /// The filter is the caller's, because the two routes configure it
    /// differently: `/chat/stream` captures thinking blocks when the user asked
    /// to see them and never in voice mode, `/agent/chat/stream` never does.
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

    /// A tool the model asked for as Harmony text markup rather than through
    /// the structured protocol, executed by the handler as a fallback.
    ///
    /// It never produced an `AgentStreamEvent::ToolCall`, so the timing has to
    /// be recorded by hand or the turn reports no tool at all.
    fn note_fallback_tool(&mut self, name: &str, latency: std::time::Duration) {
        self.last_tool_name = Some(name.to_string());
        self.last_tool_latency_ms = Some(latency.as_millis() as u64);
    }

    /// Fold one engine event into the turn and say what the stream should emit.
    ///
    /// **This is the only match on `AgentStreamEvent` in this file, and that is
    /// the point of it** — `stream_handler_parity.rs` fails if a second one
    /// appears. A new variant is written here once, and both routes carry it.
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
            // Its own event type so the UI can offer a one-click continue
            // instead of leaving the engine's "would you like me to continue?"
            // as an unanswerable sentence.
            AgentStreamEvent::TurnLimitReached { max_turns } => StreamStep::Frame(
                json!({"type": "turn_limit_reached", "max_turns": max_turns}).to_string(),
            ),
            // PAI-6 P6. A frame and NOTHING else: no `full_text`, no
            // `tool_results`, no timing slot. Those three fields are what this
            // struct exists to persist, and invariant 4 says a subagent's
            // activity never becomes the parent's history — only its final
            // result does, which arrives as the `delegate` tool's ToolResult
            // through the arm above. `absorb_progress_leaves_the_turn_untouched`
            // is the guard.
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

/// Shared SSE pipeline used by both `chat_stream` and `run_recipe`.
///
/// Callers handle activity touching, body parsing, and semaphore acquisition;
/// this helper owns the full agent turn — session creation, system-prompt
/// build, llamafile startup wait, ThoughtFilter, telemetry, memory extraction —
/// and emits the same SSE event shape regardless of entry point.
///
/// `device` is separate from `req` on purpose (PAI-1 P9): `ChatRequest` is
/// deserialised from a client-controlled body, and the paired-device rung
/// outranks every other identification the pond can make. Passing it beside the
/// body keeps the two provenances apart in the type signature, so a future
/// author cannot reach for `req.device_id` because there is nothing to reach
/// Shared SSE pipeline used by both `chat_stream` and `run_recipe`.
///
/// Callers handle activity touching, body parsing, and semaphore acquisition;
/// this helper owns the full agent turn — session creation, system-prompt
/// build, llamafile startup wait, ThoughtFilter, telemetry, memory extraction —
/// and emits the same SSE event shape regardless of entry point.
///
/// `device` is separate from `req` on purpose (PAI-1 P9): `ChatRequest` is
/// deserialised from a client-controlled body, and the paired-device rung
/// outranks every other identification the pond can make. Passing it beside the
/// body keeps the two provenances apart in the type signature, so a future
/// author cannot reach for `req.device_id` because there is nothing to reach
/// for.
///
/// The turn no longer *is* this response body. It is a task driving a
/// [`crate::runs::RunHandle`], and what is returned here is a subscriber to it
/// — see [`spawn_run`]. For this entry point the policy is
/// [`RunPolicy::Ephemeral`], which reproduces the previous contract exactly:
/// when the last subscriber leaves, the turn is cancelled.
fn chat_stream_inner(
    state: Arc<AppState>,
    permit: tokio::sync::OwnedSemaphorePermit,
    req: ChatRequest,
    device: ProvenDevice,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let run = new_run(&req, &device, crate::runs::RunPolicy::Ephemeral);
    spawn_run(state, permit, run, None, req, device)
}

/// Build a handle for a turn that has not started yet.
///
/// The session id is minted HERE rather than inside the turn, because the
/// registry indexes runs by session and a client that restarted knows its
/// session id and nothing else.
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

/// Start the turn as an owned task and return an SSE body attached to it.
///
/// `run_permit` is the detached-run cap, held for the life of the TASK rather
/// than the life of the response body. `permit` is the interactive
/// `sse_semaphore` one and is held only while somebody is reading, which is
/// what that semaphore's own documentation says it is counting.
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
///
/// This is the whole of what `chat_stream_inner`'s generator used to be, moved
/// rather than copied — `stream_handler_parity.rs` forbids a second `match` on
/// `AgentStreamEvent`, and the ordering it guards (scope before `ChatService`,
/// persistence before the `done` frame) is preserved here unchanged. The only
/// mechanical difference is that every `yield` is now a push onto the run.
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
    // Stamp the inactivity clock again now the work is actually over.
    //
    // `note_user_activity` is called when the REQUEST ARRIVES (`:1400`), and
    // was called nowhere else on this path — so the clock measured time since
    // the turn STARTED, and a turn that outran a threshold made the pond look
    // idle while it was still generating. `summary_idle_secs` defaults to 120,
    // and a fresh Orin turn in `jetson-bakeoff-2026-09-08` took 327 s: the
    // rolling-summary refresh would start its own provider call, on the same
    // single-slot engine, in the middle of the user's answer. Its
    // activity-watcher cannot save it either, because nothing re-stamps the
    // clock during a turn, so the watcher sees no resumption to cancel on.
    //
    // Every other reader of this clock inherits the fix: consolidation,
    // titling, index maintenance and proactive review all ask the same
    // question and all meant "since the pond last did something", not "since
    // it last started doing something".
    state.note_user_activity().await;
    // `finish` keeps the first terminal state, so a cancel that landed while
    // the tail was still running is not relabelled as an ordinary finish.
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

    // Ensure session exists
    if storage.get_session(&session_id).await.is_err() {
        if let Err(e) = storage.create_session(session_id.clone()).await {
            let data = json!({"error": format!("Failed to create session: {}", e)}).to_string();
            run.push(data, true);
            run.finish(crate::runs::RunState::Failed);
            return;
        }
    }

    let settings = state.settings_repo.get().await.unwrap_or_default();

    // No system prompt is built here. It used to be, into a `_system_prompt`
    // that nothing read: `AgentRequest` has no system-prompt field, and the
    // adapter builds the real one from `build_prompt_partition`. The block cost
    // a disk read of `<data_dir>/prompts/system.md`, a Tera render of the 8 KB
    // balanced template, an `Agent::list_tools` round trip and a tool-guidance
    // format -- every turn, and on every recipe run -- and then dropped all of
    // it. Editing that file to change behaviour changed nothing, which is the
    // worse cost: it read as a working override.
    //
    // Two things rode on it and are therefore inert until deliberately rewired:
    // `AppState::prompt_template_dir` (the file override above, superseded by
    // the template repo the adapter reads) and `AppState::mcp_memory`, whose
    // instructions were appended here and nowhere else.

    let model_role = "chat";

    // The same verdict the AgentRequest carries, so a turn's memory is
    // WRITTEN under the identity it was READ under. Extraction stamps
    // `profile_id` from this; before it, every fragment was unattributed
    // and `Owner(id)` reads matched exactly what `Household` did.
    let turn_scope = resolve_turn_scope(state, &session_id, &device).await;

    let mut chat_service = pond_core::shared::services::chat::ChatService::new(
        state.agent.clone(),
        session_id.clone(),
        storage.clone(),
    )
    .with_profile_scope(turn_scope.clone())
    // PAI-5 P6. The user's own choice, off by default. The `Thinking` arm
    // below calls `record_thinking` unconditionally; this is what decides
    // whether anything comes of it.
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
    // Phase F2: the images go in with the message so a follow-up turn can
    // still see them after a trim, a compaction rebuild, or a restart.
    // The id is kept because this row lands BEFORE inference starts. A turn
    // cancelled before it says anything would otherwise leave a question with no
    // answer under it — see the repair below.
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
    // If any role uses llamafile and the process is not responding, emit a
    // status event and wait up to 90 s before attempting to stream.
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

    // The turn's own state: the visible answer, the tool results that go
    // with it, and the per-tool timing this route reports as TurnMetrics.
    // The filter strips Harmony-style `<|channel>thought ... <channel|>`
    // preambles and `<think>…</think>` blocks out of the per-token stream;
    // when show_thinking is enabled it captures them as SSE events instead.
    // Voice mode always disables thinking capture.
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
    let mut cancelled = false;

    loop {
        // Cancellation used to be the response body being dropped: the generator
        // went with it, taking `agent_stream` and firing the adapter's own
        // `DropGuard`. The turn is a task now, so that no longer happens by
        // itself and has to be observed. `biased` so a cancel that arrives with
        // events already queued still wins -- a voice barge-in that kept
        // generating for another two sentences would be no barge-in at all.
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
                // Progress observed — extend the idle window.
                deadline = tokio::time::Instant::now() + idle_budget;
                match event_result {
                    Ok(event) => {
                        match turn.absorb(event) {
                            StreamStep::Nothing => {}
                            StreamStep::Frame(data) => {
                                run.push(data, false);
                            }
                            StreamStep::Reasoning { frame, block } => {
                                // PAI-5 P6. Offer it to the persistence
                                // owner; `record_thinking` drops it unless
                                // the user turned `persist_thinking` on.
                                // The SSE frame is unchanged either way --
                                // showing it live and keeping it are
                                // different consents.
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
                                // The `done` frame for this route is emitted
                                // at the very end, after persistence and
                                // telemetry, and carries the usage totals.
                                continue;
                            }
                        }
                        // Emit captured thinking blocks as SSE events (when show_thinking is on)
                        for thinking_content in turn.thought.take_thinking() {
                            let data = json!({"type": "thinking", "content": thinking_content})
                                .to_string();
                            run.push(data, false);
                        }
                        // After every push the filter may have captured a complete
                        // tool-call envelope (`<|tool_call> ... <tool_call|>`).
                        // The model emitted tool calls as Harmony text markup instead of
                        // the structured protocol. Execute them directly as a fallback.
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
                run.push(data, false);
                break;
            }
        }
    }

    // Explicit, and this line is the cancellation. Dropping the agent stream
    // fires the `DropGuard` the adapter holds inside it, which cancels the token
    // handed to the agent loop and releases the authority lease and the device
    // claim beside it. Letting scope do it would run everything below -- the
    // review's second model call included -- while still holding that claim.
    drop(agent_stream);

    // Flush any tail buffered by the thought filter (e.g. text after the
    // last `<channel|>` that had not yet exceeded the safe-emit threshold).
    if !timed_out && !cancelled {
        let tail = turn.thought.flush();
        if !tail.is_empty() {
            turn.full_text.push_str(&tail);
            let data = json!({"type": "text", "content": tail, "token": tail}).to_string();
            run.push(data, false);
        }
    }

    // ── Adversarial answer review (post-inference) ───────────────
    // When review_mode is "on", evaluate the answer before persisting.
    // If the reviewer rejects it, revise and emit a review_revision
    // event that the frontend uses to replace the text.
    // Skipped when the agent timed out — no point reviewing a partial answer.
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
    // `persist_assistant_turn_with_extraction` owns both concerns: it
    // writes tool results / assistant text / usage to session_messages,
    // then spawns memory extraction in the background. The handler cannot
    // accidentally omit extraction by refactoring this block.
    // Nothing was said, and nothing is coming.
    //
    // The user's message was committed before inference began. Leaving it shows
    // a question the pond visibly never answered, and hands the NEXT turn a
    // prompt ending on a user line nothing replied to. `delete_messages_from`
    // is the same primitive the edit-and-resend path uses.
    //
    // Deliberately narrow. A turn that produced ANY text or tool result keeps
    // both halves — a barge-in should not erase the sentence the user heard.
    // A TIMEOUT keeps them too: the same words are worth retrying, and the
    // error frame already said what happened.
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

    // The turn's context window, resolved ONCE.
    //
    // Two consumers follow — per-turn telemetry and the context-growth
    // monitor — and they used to resolve it independently. Telemetry went
    // through `ContextGovernor`, whose documented precedence is
    // EngineReported > Registry > CatalogRecord > Override > Heuristic. The
    // monitor forty lines below took `context_window_override` when set and
    // `capabilities().context_window_tokens` otherwise, which inverts that
    // order and never consults the registry pin at all. On a Jetson pinned to
    // 4096 with capabilities reporting 32768, the monitor read utilisation at
    // roughly an eighth of the truth, so `context_warning` could not fire
    // before the trimmer started dropping turns. Resolving once is the only
    // way the two can be guaranteed to agree.
    // Rung 3's input: the catalog row for the active chat model. Reached
    // through `state.model_repo`, which is the same catalog the Models tab
    // lists — so telemetry, the growth monitor and the UI are all quoting
    // one number. Absent repo or absent row falls through to the rungs
    // below, which is what happened for every turn before PAI-3 P3b.
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
        // Not reachable from the API layer; the adapter owns the registry
        // lookup and reports the result via TurnStats.
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

            // Estimate turn number from existing telemetry for this session.
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
                // PAI-3 / PAI-4 read this. The accumulator gathers it from
                // the `ToolCall` / `ToolResult` pair and from the Harmony
                // fallback above; losing it is a silent regression in a
                // different workstream, not a cosmetic one.
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
                // What thinking cost, and what it cost when it went wrong.
                // `reasoning_tokens` stays Option all the way down: a turn
                // from a provider that reports no stats has not been
                // measured, and that is a different fact from a turn that
                // did no thinking. `reengagements` is a plain count on
                // `TurnStats`, so the only Nones here are turns with no
                // stats at all.
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

                // PAI-4 P6: and then actually do something about it. Until
                // this phase the frame above was the entire response to a
                // filling context window — the server warned the client and
                // took no action itself.
                //
                // This call spawns and returns; it must stay that way. We
                // are inside the SSE generator, the `done` frame below is
                // still unsent, and invariant 1 is that compaction never
                // blocks a turn, ever. Doing the work here — the obvious
                // reading of "act on should_compact" — would put a
                // summarisation model call between the user's last token and
                // the end of their stream, on the tier that can least afford
                // it. Everything real happens in the detached task.
            }
        }
    }

    // Record how the turn actually ended BEFORE the terminal frame goes out, so
    // a client that reads `state` from the discovery route and a client reading
    // the frame cannot disagree. `finish` keeps the first terminal state, so the
    // cancel path below does not overwrite a state an explicit stop already set.
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

    // Done event
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

/// One subscriber: replay what it missed, then follow the live tail.
///
/// The same shape `notifications_stream` uses, and for the same reason. The
/// ordering of the two lines that open it is load-bearing: subscribing BEFORE
/// reading the snapshot is what stops a frame produced between the two from
/// being missed by both paths.
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
        // Held only while somebody is reading. The turn does not care.
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

        // The ring rolled past where this client was. Say so: it has genuinely
        // lost frames, and reloading the session is the only honest recovery.
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
                // Not an error on the wire, and this is what the ring is for:
                // it holds strictly more than the broadcast queue can drop, so
                // everything this subscriber missed is still there to re-read.
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
                // The sender lives on the handle in the registry, so this means
                // the handle went away underneath an attached client. Never go
                // silent about it.
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

/// May this caller follow this run?
///
/// Checked against the owner captured when the turn STARTED, not against
/// whoever happens to be asking. A run's frames can carry a household member's
/// private content, so the paired-device rung is not relaxed here — see
/// `RunOwner` for why an unattributed run is not a widening.
fn may_reattach(run: &crate::runs::RunHandle, device: &ProvenDevice) -> bool {
    match &run.owner {
        crate::runs::RunOwner::Device(owner) => device.id() == Some(owner.as_str()),
        crate::runs::RunOwner::Unattributed => true,
    }
}

/// Find the run, or say why not.
///
/// An unknown run and a run belonging to somebody else answer the same 404, so
/// the endpoint is not an oracle for which run ids exist. The distinction is
/// logged, not returned.
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

    // A run id minted under another epoch names a run that died with the last
    // process. Saying so is the difference between an honest "reload the
    // session" and a 404 the client cannot tell apart from "it aged out".
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

    // `Last-Event-ID` is what a browser resends on its own; `after_seq` is what
    // survives a process restart, where nothing browser-managed does. Both are
    // honoured and the further-along one wins, because replaying a frame the
    // client already has is the harmless direction to be wrong in.
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

/// Stop a run on purpose.
///
/// Necessary because dropping the connection no longer means "stop" for a
/// detached run. Idempotent: asking twice, or asking for one that already
/// ended, reports the state rather than failing.
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

/// Fire the token, and never `JoinHandle::abort()`.
///
/// An abort at an arbitrary await point skips the persistence tail, which is
/// the whole reason a detached turn is worth having: a cancelled turn must
/// still write down what it already said. Cooperative cancellation only.
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
        //
        // Only queried when it is actually needed. A titled session — which is
        // nearly all of them once the naming pass has run — skips it, and on a
        // pond with hundreds of conversations this list is per-session queries
        // all the way down.
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

        // The card's preview is what the pond ANSWERED, not what it was asked.
        // The title already carries the question, and a card whose heading and
        // body paraphrase the same sentence reads as a rendering fault.
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

/// The pond's first answer in a conversation, for a history card.
///
/// Longer than [`derived_session_label`] because the two answer different
/// questions: the label stands in for a missing title and says what the
/// conversation was *about*, this sits underneath a title and says what came
/// back. Deliberately the assistant's words rather than the user's — the title
/// already carries the question, and a card whose heading and body paraphrase
/// each other reads as a rendering fault.
///
/// Capped well past what any card shows so the client can fade the overflow
/// out rather than ending on an ellipsis mid-card.
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
///
/// # The reasoning total, and what it costs
///
/// PAI-5 P2's other display surface. `total_prompt_tokens` and
/// `total_completion_tokens` are running counters on the `sessions` row, kept
/// by `increment_usage`; there is no such counter for reasoning, so the total
/// here is summed from the `session_messages` rows — an O(corpus) read on a
/// route that was O(sessions). I took that cost deliberately and it should not
/// survive: the fix is one `SessionStorage` method
/// (`reasoning_token_totals() -> (u64, u64)`) answering both numbers with one
/// `SELECT SUM(reasoning_tokens), COUNT(reasoning_tokens)`, which this handler
/// would then call instead of walking sessions. That belongs to whoever next
/// owns `pond-core/src/user_data/ports/session_storage.rs`.
///
/// **`counted_reasoning_turns` is not decoration.** `total_reasoning_tokens: 0`
/// is ambiguous between "the models did no thinking" and "nothing counted", and
/// on a pond driven only from the desktop the honest answer is the second: both
/// HTTP stream handlers persist through `ChatService::persist_assistant_turn`,
/// whose usage argument is a `(prompt, completion)` tuple that cannot carry a
/// third number, so their rows keep NULL. The count is carried end to end only
/// on the `ChatService` path — `pond chat` and the terminal voice loop. A zero
/// beside a zero count says "nobody counted"; a zero beside a non-zero count
/// says the turns really did no thinking, which is what PAI-5 P5 needs to read.
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
        // A session whose messages cannot be read contributes nothing rather
        // than failing the whole summary: the two provider counters above are
        // already answerable and refusing to report them because one session's
        // rows are unreadable trades a complete answer for none.
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
        // Deliberately NOT folded into `total_tokens`. That figure is what the
        // cloud prices above are multiplied by, and this one is GIAP's own
        // estimate over the thinking text rather than anything a provider
        // billed for -- adding it would put an estimate inside a cost.
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

    // Engine cleanup MUST run before the pond row (and the engine_session_map
    // pairing it cascades away) is deleted below — once the pairing is gone,
    // the engine-side session id is unrecoverable and its messages, usage
    // ledger, and inline image attachments become permanently unreachable
    // garbage in a store the REST API never reads.
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

    // PAI-4 P6 — the "on clear" half. `reset_session` had no production caller
    // at all before this phase, so the growth map only ever grew: every session
    // the process had ever streamed a turn for stayed in it, and a new
    // conversation created under a recycled id would have inherited the
    // utilisation, the growth samples and the compaction cooldown of the one it
    // replaced. Deleting the row is the only place the session genuinely stops
    // existing, so it is the only place a full reset is the right call.
    state.context_monitor.reset_session(&session_id);

    Ok(StatusCode::NO_CONTENT)
}

/// Delete a message and every later message in the same session.
///
/// DELETE /api/v1/sessions/:session_id/messages/:message_id
///
/// This is the "edit"/"refresh" primitive, not a general message-delete: the
/// client truncates the conversation from a user message onward, then
/// resubmits (unchanged for refresh, edited for edit) as a normal new turn
/// through `/chat/stream` — no separate regenerate code path needed.
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

    // Truncating `pond_system.db` is only half of forgetting a turn, and the
    // half nobody sees. The live engine session still holds the deleted
    // messages, so without this the model keeps being shown the exact turns the
    // user just removed: an edit re-answers with the old answer in context, a
    // regenerate is asked to regenerate something it can still read, and the
    // two stores diverge for the life of the process. The user's only signal
    // that any of that happened is an assistant that seems not to have noticed.
    //
    // `forget_session` drops the GIAP->engine pairing rather than replaying a
    // deletion into the engine. That is deliberate: the next turn re-resolves
    // the pairing and hydrates a fresh engine session from pond history, which
    // IS the truncated history, so the two stores converge on the one that is
    // authoritative instead of both being edited and hoping they agree. It is
    // also a no-op when no pairing exists, so a first-turn edit costs nothing.
    state.agent.forget_session(&session_id).await;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MessageFeedbackRequest {
    /// `true` = liked (keep as training data), `false` = disliked (excluded),
    /// `null`/omitted = clear any prior vote.
    #[serde(default)]
    liked: Option<bool>,
}

/// Set or clear the like/dislike training-feedback flag on one message.
///
/// PUT /api/v1/sessions/:session_id/messages/:message_id/feedback
/// Body: `{ "liked": true | false | null }`
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

    // No `offset` in the query: honor the doc comment above — "most recent"
    // means newest-first via get_recent_messages, not the oldest page that
    // get_messages_paginated(limit, 0) would return. Callers that DO pass
    // `offset` are doing old-style forward pagination and keep that behavior.
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

    // Phase F2. One cheap metadata query for the whole page (no bytes read),
    // grouped by message id. Absent for sessions that never had an attachment,
    // and a storage error here must not fail a history read.
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

    // PAI-5 P6. One query for the whole page, grouped by message id, exactly
    // like the attachments fold above. Empty for every session recorded before
    // `persist_thinking` was turned on, and a storage error must not fail a
    // history read -- reasoning is a convenience, the transcript is not.
    //
    // THIS IS THE ONLY PRODUCTION CALLER of `get_thinking_for_session`, and
    // `crates/pond-core/tests/thinking_is_never_replayed.rs` fails the build if
    // a second one appears anywhere that builds a prompt. Nothing here reaches
    // the model: the value is serialised into the HTTP response and dropped.
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
            // Phase F2: attachments are referenced, never inlined. Base64 in a
            // history read would turn a routine page load into megabytes; the
            // client fetches each image once from the URL below and the browser
            // caches it.
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
            // PAI-5 P6: the reasoning that produced this reply, if the user
            // chose to keep it. Absent (not `[]`) on every message that has
            // none, so the client can tell "not kept" from "kept nothing".
            if let Some(blocks) = thinking_by_message.get(&m.id) {
                obj["thinking"] = json!(blocks);
            }
            obj
        })
        .collect();

    // PAI-4 P4: opening a session is the resume signal. Never awaited — see

    Ok(Json(json!({ "messages": list })))
}

/// The body every outcome of `POST /sessions/:id/compact` reports.
///
/// The context block is deliberately the same four fields the `context_warning`
/// SSE frame carries, read out of the same [`ContextHealth`], so the number on
/// the button and the number the stream pushed cannot drift into disagreeing
/// about the same session. `turns_remaining` is the one departure: the frame
/// serialises `u32::MAX` when growth is unknown, which reaches a client as
/// 4294967295 and reads as a number. Here it is `null`.
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

/// POST /api/v1/sessions/:session_id/compact — compact this session now.
///
/// PAI-4 P7. The third and last trigger: P4 is the time axis (a session reopened
/// after a gap), P6 the pressure axis (a session past 75% of its window), and
/// this one is a person deciding. It runs the same pass as both of them —
/// `run_compaction_pass` — and it is subject to the same rules, which is the
/// whole of the design and the only part that is easy to get wrong.
///
/// **It took P6's rate limiter, and that was the bug — PAI-4 P7b-fix.** The
/// original shape refused the press unless `ContextMonitor::claim_compaction`
/// granted, on the argument that `should_compact` is monotone above 75% and an
/// ungated endpoint would let anything holding a bearer token queue a
/// summarisation between every pair of turns. The argument is sound and the
/// implementation could never work, because the two axes shared one quota and
/// the pressure axis always took it first: in the chat-stream generator the
/// `context_warning` frame is yielded and `spawn_pressure_compaction` runs one
/// statement later inside the same `if health.should_compact` block, while the
/// button that frame renders only appears after `done`. Six consecutive
/// pressured turns produced six `cooling_down` refusals and never one pass, so
/// the advertised success path was unreachable rather than uncommon.
///
/// So this now calls `ContextMonitor::claim_manual_compaction`, which differs
/// from `claim_compaction` in exactly one respect: it skips the turn cooldown.
/// It still requires pressure, and it still *stamps* the cooldown — a press
/// consumes the automatic axis's quota without checking it, so the two together
/// can never buy two summarisations for one turn. That is why this is not a
/// widening: the manual axis gains nothing the pressure axis did not already
/// have, it only stops being refused by a claim taken on its behalf.
///
/// **What actually bounds repeated presses, since the cooldown no longer does.**
/// Two things, and neither is new. `compaction_in_flight` below refuses while a
/// pass is running, which is what protects a serial on-device engine. And
/// `SessionSummaryService::refresh` decides `NothingToDo` from the rolling
/// summary's through-pointer and the message count *before* it reaches
/// `provider.complete`, so a second press with no new turns in between costs a
/// database read and no model call — and every new message is a turn the person
/// had to take. The large tier's `resummarise` is the one path that can spend a
/// call per press; it is gated on `ModelClass::permits_compaction_model_call()`,
/// i.e. not the on-device engine this argument is about, and it shares this
/// pass's single in-flight claim.
///
/// **What it still costs.** A session below the compaction threshold answers
/// `not_under_pressure` rather than compacting, because the claim recomputes
/// `should_compact` under its own lock and refuses. A session the monitor has
/// never recorded a turn for — anything from before this process started, and
/// everything at all when `context_monitor_enabled` is off — is in that same
/// state, because utilisation is only ever learned from a turn. Both answer with
/// a reason rather than a silent no-op, which is the least a manual control
/// owes. Making the button work under no pressure would need a wall-clock rate
/// limit that does not exist; that is a phase, not a line, and it is still not
/// this one.
///
/// **This one awaits.** P4 and P6 spawn and return because they run inside a
/// request a user did not make — a reopen, and an SSE generator with the `done`
/// frame still unsent — and invariant 1 says compaction never blocks a turn.
/// Neither applies here: this request *is* the compaction, nobody is watching a
/// token stream, and awaiting is the only way to report what the pass did. The
/// turn-preemption watcher inside `run_compaction_pass` still holds, so a user
/// who presses the button and then starts typing cancels their own pass and
/// persists nothing.
///
/// Everything short of a server fault answers 200 with a `status`/`reason` pair
/// rather than an error code: "I did not compact, and here is why, and here is
/// where your window actually is" is a successful answer to this question. 404
/// is reserved for a session id that does not exist, so a typo is not reported
/// as a healthy window.
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

    // Read before the claim, and in this order, because each answers a
    // different question the user might be asking and only the first true one
    // is worth reporting.
    if !settings.context_monitor_enabled {
        // Nothing has called `record_turn` for any session, so every utilisation
        // number below this line is a zero that means "not measured", not "empty".
        return Ok(compaction_report(
            &session_id,
            "skipped",
            Some("monitor_disabled"),
            None,
            &health,
        ));
    }
    // `hybrid_compaction_enabled` no longer gates this.
    //
    // It read "the same switch the other two axes read first", and those axes
    // are gone — the press now asks the ENGINE to compact, and the engine
    // compacts on its own threshold whatever this setting says. Refusing the
    // button while automatic compaction carries on regardless would be
    // arbitrary: the user would be told compaction is off while watching it
    // happen. The setting's remaining job is the idle rolling summary, which is
    // a retrieval artefact rather than a compaction axis.
    if !health.should_compact {
        return Ok(compaction_report(
            &session_id,
            "skipped",
            Some("not_under_pressure"),
            None,
            &health,
        ));
    }

    // C4: the press goes to the ENGINE now.
    //
    // GIAP used to own this: claim a slot, spawn an activity watcher, run its
    // own summariser, release. Since C1 the engine owns context management, so
    // the button asks it to compact rather than doing a second, different kind
    // of compaction beside it. `Agent::compact_session` calls goose's own
    // `compact_messages` with `manual_compact: true`.
    //
    // The in-flight claim, the cooldown and the activity watcher went with the
    // GIAP-side pass. goose serialises its own compaction per session, and the
    // press is explicit — a user who presses twice means it.
    let (status, reason, outcome_label) = match state.agent.compact_session(&session_id).await {
        Ok(Some(_retained)) => {
            state.context_monitor.note_compacted(&session_id);
            ("compacted", None, Some("refreshed"))
        }
        // The backend has no manual compaction, or the session had nothing to
        // compact. Reported as skipped rather than as success.
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

    // Re-read: `note_compacted` drops the growth samples, so a compacted session
    // reports `turns_remaining: null` here rather than a rate measured against a
    // history that no longer exists. That is the post-state the caller asked for.
    let after = state.context_monitor.check_context_health(&session_id);
    Ok(compaction_report(
        &session_id,
        status,
        reason,
        outcome_label,
        &after,
    ))
}

/// POST /api/v1/sessions/retitle — rename conversations now, without waiting
/// for an idle window.
///
/// The attended counterpart to the background pass in `pond-server`. It skips
/// the *scheduling* gate only: you asked for it, so the pond does not argue
/// about whether now is a good moment, and it does not abandon the run when you
/// keep typing. Every per-conversation rule still applies —
///
/// - a name somebody typed is never overwritten,
/// - a conversation too short to describe is left to the six-word fallback,
/// - a model-written name that still fits its conversation is not rebuilt just
///   to spend a model call arriving at the same words.
///
/// Deliberately independent of `session_titling_enabled`. That setting governs
/// whether the pond does this *unattended*; pressing a button is not that, and
/// a control that silently does nothing because of a switch somewhere else is
/// the worse surprise.
async fn retitle_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    use pond_core::shared::domain::session_activity::SessionOrigin;
    use pond_core::shared::services::session_title::{
        RetitleOutcome, SessionTitleService, SkipReason,
    };

    // A manual pass is bounded too. On a small board every rename is a model
    // call, and a request that walks 400 conversations is a request that times
    // out. `capped` tells the caller another press will pick up where this one
    // stopped.
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
        // The pond's own background conversations are not things anybody
        // browses, so naming them spends a model call on a row nobody reads.
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
            // One bad conversation must not sink the whole pass.
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
///
/// Unlike the sweep, this one obeys rather than protects. The sweep's rules
/// exist because it touches conversations nobody is looking at; a click on the
/// conversation in front of you is consent about that conversation, so this
/// replaces a name that still fits and a name typed by hand alike.
///
/// The one refusal it keeps is a conversation too short to describe, where the
/// obstacle is that there is nothing to say rather than permission to say it.
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
        // Reported rather than swallowed: a button that declines must say so,
        // or it is indistinguishable from one that is broken.
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

/// Percent-encode the few characters that would break a path segment.
///
/// Session and attachment ids are UUIDs in practice, but `session_id` is
/// caller-supplied on `/chat/stream`, so a returned URL must not be able to
/// smuggle an extra path segment or a query string.
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

/// Serve one persisted image attachment's raw bytes (phase F2).
///
/// Bytes, not base64: the client uses this straight as an `<img src>`, and
/// re-encoding only to have the browser decode again is pure waste. The
/// `session_id` path segment is checked against the stored row so an attachment
/// id from one conversation cannot be read through another's URL.
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
                // Content-addressed by a random id and never rewritten, so it is
                // safe to cache hard. Saves re-fetching every image on every
                // history load.
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

/// The LAN address a phone on the same network should use to reach this hub.
///
/// A mDNS hostname is the nicer thing to hand out — it survives a DHCP lease
/// change, where a baked-in address does not — but Android's resolver does not
/// do mDNS, so `<host>.local` simply fails to resolve there. The pairing QR
/// carries both and lets the client fall back.
///
/// Found by asking the routing table which source address it would use to reach
/// the mDNS group, which is the same question the phone is really asking. No
/// packet is sent: `connect` on a UDP socket only fixes the route. That is also
/// why the destination is the multicast group rather than a public address —
/// routing to the internet may well go out of a VPN, which is the one interface
/// that cannot carry LAN discovery.
///
/// Returns `None` rather than a guess when there is no LAN route to speak of.
fn lan_address() -> Option<String> {
    use std::net::UdpSocket;

    // The mDNS group first, then RFC1918 gateways for hosts whose multicast
    // route is unusual. Each is only a routing probe.
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

/// True for the CGNAT range Tailscale assigns node addresses from, 100.64.0.0/10.
///
/// Checked explicitly because the probe below can succeed without a tailnet: a
/// host with an ordinary default route will happily tell you which address it
/// would use to reach 100.100.100.100, and that answer is its LAN address. Only
/// an address inside this range is evidence the packet would go over WireGuard.
fn is_tailnet_v4(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (64..=127).contains(&b)
}

/// This Pond's Tailscale address, when it is on a tailnet.
///
/// The address a phone uses to reach this Pond from outside the house. It is
/// worth having alongside `lan_address` rather than instead of it: at home the
/// LAN address is a direct hop and needs no VPN running, so the client prefers
/// it and falls back to this one.
///
/// Found the same way as `lan_address` — ask the routing table which source
/// address it would use, sending nothing — but aimed at 100.100.100.100, the
/// address Tailscale's own MagicDNS resolver answers on. Note the symmetry with
/// the comment above: `lan_address` avoids routing to the internet precisely
/// because it may leave via a VPN, and here that VPN is the whole point.
///
/// Deliberately not the MagicDNS *name*: reading that means shelling out to the
/// `tailscale` CLI, and a node's address is stable for its lifetime, so the name
/// buys nothing here. The certificate work will need it and can pay for it then.
fn tailnet_address() -> Option<String> {
    use std::net::UdpSocket;

    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("100.100.100.100:53").ok()?;
    let std::net::IpAddr::V4(v4) = socket.local_addr().ok()?.ip() else {
        return None;
    };

    is_tailnet_v4(v4).then(|| v4.to_string())
}

async fn system_info(
    State(state): State<Arc<AppState>>,
    transport: Option<axum::Extension<crate::network::CompanionTransport>>,
) -> Json<Value> {
    let (https_port, tls_spki_sha256) = crate::network::transport_fields(transport);
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let hostname = hostname
        .strip_suffix(".local")
        .unwrap_or(&hostname)
        .to_string();

    Json(json!({
        "hostname": hostname,
        // Null when the host has no LAN route. Clients that cannot resolve
        // `<hostname>.local` — Android, notably — use this instead.
        "lan_address": lan_address(),
        // Null unless this Pond is on a tailnet. Reachable from outside the
        // house, so it is what a paired phone falls back to when the LAN
        // address does not answer.
        "tailnet_address": tailnet_address(),
        "https_port": https_port,
        "tls_spki_sha256": tls_spki_sha256,
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

/// `GET /api/v1/matter/status` — what the Matter integration is actually doing.
///
/// The Devices tab polls this after toggling Matter: enabling installs and
/// starts a controller, which takes long enough that the UI has to show
/// progress rather than pretend the save was the whole story.
async fn matter_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let status = match &state.matter {
        Some(matter) => matter.status().await,
        // No Matter support wired at all. Reported as plain "off" — from the
        // user's side there is nothing to distinguish, and nothing to fix.
        None => pond_core::user_data::ports::matter_runtime::MatterStatus::disabled(),
    };
    Json(serde_json::to_value(status).unwrap_or_else(|_| json!({})))
}

/// The live commissioner, or the error explaining why there isn't one.
///
/// "Off" and "on but the controller is unreachable" used to collapse into a
/// single "Matter is not enabled" 503, which sent users to look for a switch
/// that was already on. They are kept apart here so the message names the thing
/// that is actually wrong.
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
        // Reachable only on a build without the Matter feature, since the
        // toggle that used to produce this state is gone. Naming the build is
        // the only actionable thing left to say: there is no switch to point at,
        // and telling someone to find one costs them the trip.
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
        // Connected without a commissioner means the runtime was torn down
        // between the two reads. Transient by nature, so it reads as such.
        MatterState::Connected => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "The Matter controller connection just dropped. Try again in a moment."
            })),
        ),
    })
}

/// `POST /api/v1/devices/commission` — bring a Matter device onto the fabric.
///
/// Distinct from `register_device` on purpose: a Matter device is not GIAP's to
/// name until it has joined the fabric. Optionally takes a `name`, which is
/// written to the device's NodeLabel and stored as its registry name so chat
/// resolution ("turn on the living room light") matches it.
///
/// The registry row is ensured here from the commission response rather than
/// left to the bridge: the bridge's discovery is asynchronous, so relying on it
/// would race the user's chosen name. Registering by device id is idempotent —
/// whichever of the two runs first, the other sees the row and does not
/// duplicate it.
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
    // Validated before it reaches the controller.
    let code =
        pond_core::user_data::ports::device_commissioning::parse_setup_code(raw).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
        })?;

    // Optional user-chosen name, trimmed; empty is treated as absent.
    let name = req
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let device = commissioner
        .commission(code, name.clone())
        .await
        // `{e:#}`, not `to_string()`: the latter prints only the outermost
        // context, so every failure reached the user as a bare "commissioning
        // failed" while the reason it was wrapped around — the controller's own
        // error — was dropped on the floor.
        //
        // The commissioner is what keeps this from being the opposite problem.
        // It renders the sentence a user can act on and returns that alone, so
        // there is no bookkeeping frame here for `{:#}` to print as if it were
        // prose. This endpoint names the operation; the message says what went
        // wrong with it.
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("{e:#}")})),
            )
        })?;

    // Ensure the registry row exists with the intended name, regardless of
    // whether the bridge got there first.
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

/// The devices a Matter hub speaks for, by id, plus the hub itself.
///
/// The trailing dash is load-bearing: without it `matter-9-` would also match
/// `matter-90`, and deleting one hub would take an unrelated one's children with it.
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

    // A bridged device is one endpoint of a hub that speaks for several. Matter
    // commissions NODES, so there is no fabric operation that removes one endpoint —
    // and GIAP does not own the hub's child list either; the hub's own app does.
    //
    // So there are only three things this could mean, and two of them are wrong.
    // Decommissioning acts on the node, so it would silently remove every sibling
    // and the hub. Dropping the row alone leaves the controller to re-announce the
    // device on its next subscribe — the zombie this endpoint's own comment below
    // warns about. Refusing and saying why is the honest one.
    if matter_bridged_endpoint(&id).is_some() {
        let hub_id = matter_node_id(&id)
            .map(|node| format!("matter-{node}"))
            .unwrap_or_default();
        // Name the hub as the user knows it, not by its id.
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

    // A Matter device must leave the fabric before its row is removed, or the
    // controller re-announces it on the next start_listening and it reappears.
    // If we cannot reach the controller to do so, the delete is refused rather
    // than half-applied.
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

    // Removing a hub from the fabric removes everything behind it, so its children's
    // rows have to go too or they linger as devices that will never heartbeat again
    // and can never be deleted (the branch above refuses them, correctly, and their
    // hub no longer exists to delete instead).
    //
    // Children first, hub last: a failure part-way then leaves the hub visible and
    // re-deletable rather than orphaning its children.
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

// ── Private mesh (#132) ──────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct AddMeshPeerRequest {
    peer_id: String,
    trust_scope: String,
    /// Dial hint for a best-effort connect right after trust is recorded.
    /// Not persisted by `PeerDirectory` — future reconnects rely on
    /// Kademlia rendezvous or the user re-sharing an invite.
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

/// `GET /api/v1/mesh/peers` — trusted peers, enriched with live connection
/// status (from `mesh_transport` if configured) and credit balance.
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

/// `POST /api/v1/mesh/peers` — trust a peer, then (best-effort, if an
/// address is given and the mesh transport is running) try to connect.
/// Trust is recorded even if the connect attempt fails — `PeerDirectory`
/// and `MeshTransport` are separate concerns.
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

/// `POST /api/v1/mesh/peers/{peer_id}/credit` — manual top-up, standing in
/// for real Lightning settlement (`pond-adapters-lightning`, not built yet).
/// `CreditLedger` is peer-agnostic (keyed by raw `PeerId`, no relationship
/// to `PeerDirectory`), so this checks trust itself — without that check the
/// route would let you silently fund a balance for someone outside your
/// circle.
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

/// `GET /api/v1/mesh/peers/{peer_id}/capabilities` — live "what does this
/// peer offer right now", queried over the mesh (not cached — see
/// `PeerCapabilityQuery`'s own docs on why this isn't `PeerDirectory` data).
/// 503 when `peer_capability_query` isn't configured (mesh feature/setting
/// off); trust is checked first, same discipline as `credit_mesh_peer`.
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

/// `GET /api/v1/mesh/settlement` — read-only status for the periodic
/// settlement job (#132 Milestone 6): the dev-decided exchange rate, and
/// each trusted peer's currently-pending usage. Deliberately read-only —
/// the rate is `MESH_SETTLEMENT_MILLISATS_PER_TOKEN`, a fixed constant, not
/// a per-Pond setting (see that constant's own docs on why).
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
        // Borrowed: what we owe (settlement pays this). Lent: what peer
        // owes us (shown for transparency only — their job to collect).
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

/// `GET /api/v1/mesh/self` — this Pond's own mesh identity + invite link.
/// Soft-disabled like weather: 200 with `mesh_enabled: false` when
/// `mesh_transport` isn't configured, not an error — the UI renders an
/// "enable mesh" prompt instead of an error state.
async fn get_mesh_self(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let transport = state.mesh_transport.read().await.clone();
    let Some(transport) = transport else {
        return Ok(Json(json!({ "mesh_enabled": false })));
    };
    let peer_id = transport.local_peer_id();
    let addresses = transport.listen_addresses().await.unwrap_or_default();
    // Prefer a routable address over loopback — an invite is only useful
    // to a peer on a different machine.
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
    // Every real push token (FCM, APNs hex, `ExponentPushToken[...]`) is
    // printable ASCII. Rejecting anything else here keeps control characters
    // out of the logs and stops malformed tokens reaching the relays at all.
    if !req.token.chars().all(|c| c.is_ascii_graphic()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "`token` must be printable ASCII" })),
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

/// Decide whether a settings save should geocode the location name into
/// coordinates, and if so, the (trimmed) name to look up.
///
/// The Settings page sends the whole settings object on every save, so the
/// latitude/longitude keys are always present (as the current values, or 0/null
/// when blank). Detecting an *explicit* coordinate edit therefore can't just
/// check for the key — it compares the patched value to what is stored. We
/// geocode when a name is present and the user did not edit coordinates, and
/// either the name changed or the coordinates are unset (0,0, the onboarding
/// default). Pure, so the decision is unit-tested without a network call.
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
    // The user explicitly set coordinates only when the patch carries a value
    // that differs from what is stored — a full-object echo of the current
    // coordinates does not count.
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

    // Load current settings so we only overwrite the fields the caller provided.
    let current = state.settings_repo.get().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to load current settings: {}", e)})),
        )
    })?;

    // Validate and canonicalise every ruled field, in pond-core.
    //
    // Three arms used to live here by hand — timezone, network_mode and
    // reasoning_effort — and they were the ONLY server-side rules the pond had.
    // The rest of the real rules were in the desktop's `validation.ts`, which
    // meant the backend stored what the browser rejected, and any writer that
    // was not the catalogue met no rule at all.
    //
    // `validate_patch` reports EVERY failing field rather than the first, and
    // canonicalises in place: `africa/nairobi` is stored as `Africa/Nairobi`,
    // `9:05` as `09:05`. A refused patch is left exactly as it arrived.
    if let Err(errors) =
        pond_core::user_data::domain::settings_validation::validate_patch(&mut patch)
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": pond_core::user_data::domain::settings_validation::render_errors(&errors),
                // Named per field as well, so a form can mark the box rather
                // than showing one sentence above the whole page.
                "fields": errors
                    .iter()
                    .map(|e| json!({ "field": e.field, "message": e.message }))
                    .collect::<Vec<_>>(),
            })),
        ));
    }

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
    // A type-invalid field must fail loudly: silently keeping `current` here
    // returned 200 with the OLD settings, so the UI flashed "Saved" while every
    // edit in the form was discarded.
    let mut merged: Settings = serde_json::from_value(base).map_err(|e| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": format!("Invalid settings value: {}", e)})),
        )
    })?;

    // The controller URL is operator-supplied and gets opened as a socket, so
    // its shape is checked here rather than at connect time — a typo should be
    // a rejected save, not a Matter section stuck reporting "unreachable".
    //
    // Only when the caller actually edited Matter: this endpoint takes a patch
    // over the whole of Settings, so validating unconditionally would let a bad
    // stored value block every unrelated save (renaming the home, changing a
    // model) until someone fixed a field they were not touching.
    // `matter_ble_enabled` counts as touching Matter: it is an argument to the
    // controller's own process, so turning it on does nothing at all until that
    // process is replaced. Left out, the toggle saved and appeared to work, and
    // BLE arrived on the next server restart -- or never, if nothing restarted it.
    let touches_matter = patch
        .as_object()
        .is_some_and(|o| o.contains_key("matter_ws_url") || o.contains_key("matter_ble_enabled"));
    let matter_url = merged.matter_ws_url.trim();
    if touches_matter && !(matter_url.starts_with("ws://") || matter_url.starts_with("wss://")) {
        // An empty address and a malformed one are different mistakes and read
        // as different sentences: "empty" tells the user the field they are
        // looking at is blank, which the placeholder otherwise hides.
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

    // Geocode-on-save: turn the location name into coordinates so the Settings
    // page shows real lat/lon and the weather gate is satisfied without the user
    // hand-entering coordinates. Onboarding saves through this same endpoint, so
    // an onboarded install ends up with real coordinates and no user action.
    // Best-effort — a failure keeps whatever coordinates were provided, since the
    // adapter resolves the name on demand anyway.
    // Write only the keys this request actually carries. Writing the whole
    // merged snapshot reverted any field another writer (the phone, the other
    // desktop UI, a model activation) changed after `current` was read.
    //
    // The patch key set is also the only trustworthy record of user INTENT:
    // these are the fields the caller deliberately sent. Keep it separate from
    // `write_keys`, which grows to include values the server derives (the
    // geocoded coordinates below) — derived is not chosen.
    let user_keys: std::collections::HashSet<String> = patch
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    let mut write_keys = user_keys.clone();

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
                // Geocoding derived these, so they must be written even though
                // the caller did not send them.
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

    // Record that these keys were deliberately chosen, so a future
    // default-adoption migration (see DEFAULT_ADOPTIONS / migration 0035)
    // leaves them alone. Runs after the write because the marker only applies
    // to rows that exist. Best-effort: the values are already saved, and
    // losing the marker only risks a later default adoption, not this edit.
    if let Err(e) = state.settings_repo.mark_user_set(&user_keys).await {
        tracing::warn!(error = %e, "failed to record user intent for settings patch");
    }

    // Revoking microphone permission has to take effect now, not at the next
    // restart — a privacy control the user has to reboot to apply is not one.
    pond_core::models::domain::mic_gate::set_mic_enabled(merged.mic_enabled);

    // Same reasoning as the mic gate directly above: a network restriction the
    // user has to restart the pond to apply is not one. Unconditional rather
    // than keyed on the patch, because it is a cheap idempotent write and a
    // missed re-install is a privacy control that silently did not take.
    pond_core::shared::services::egress::set_network_mode(
        pond_core::shared::services::egress::NetworkMode::parse(&merged.network_mode),
    );

    // Same for Matter: the toggle used to be read once at startup, so turning
    // it on changed nothing until someone restarted the Pond — which made the
    // "enable Matter first" error impossible to act on.
    //
    // Only when the caller actually edited Matter, unlike the egress gate
    // above. That gate is a cheap idempotent write; this one can restart a
    // controller. The reconciler treats "enabled but not yet Connected" as
    // needing a restart, so an unconditional send meant that ANY unrelated
    // save during the first controller install — renaming the home, changing a
    // model — tore down a multi-minute dependency install and started it again.
    // Repeat that a few times and the Devices panel sits on "Starting..."
    // forever.
    //
    // Retry from the Devices tab still works: it sends `matter_ws_url`
    // explicitly, so it is a `touches_matter` save by construction.
    if touches_matter {
        if let Some(matter) = &state.matter {
            matter.apply(pond_core::user_data::ports::matter_runtime::MatterConfig {
                url: merged.matter_ws_url.trim().to_string(),
                ble: merged.matter_ble_enabled,
            });
        }
    }

    // Same reasoning as Matter directly above: mesh_enabled used to be read
    // only at server startup, so flipping it on changed nothing until the
    // Pond was restarted. `mesh_rebuild` (set by pond-server at startup)
    // builds the real transport/provider on demand; it is always safe to
    // call — it no-ops if the stack is already built, mesh is still
    // disabled, or this binary lacks the `mesh` feature. Only when the
    // caller actually touched mesh_enabled, same discipline as
    // touches_matter: this endpoint takes a patch over the whole of
    // Settings, so calling it unconditionally would pay a settings read on
    // every unrelated save.
    let touches_mesh = patch
        .as_object()
        .is_some_and(|o| o.contains_key("mesh_enabled"));
    if touches_mesh && merged.mesh_enabled {
        if let Some(rebuild) = &state.mesh_rebuild {
            rebuild().await;
            // Pick up a real mesh provider immediately if chat_provider was
            // already "mesh" while mesh itself was still off — otherwise
            // state.llm_provider stays pinned to the UnavailableProvider it
            // cached back then, since only chat_provider/chat_model changes
            // normally trigger this rebuild (see provider_keys below).
            rebuild_llm_provider(&state, &merged).await;
        }
    }

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
                    // "mesh" (#132) has no catalog category — it borrows a
                    // trusted peer's compute, it isn't a file this Pond
                    // downloaded. Without this, `for_chat_provider("mesh")`
                    // falls into its catch-all ("llamafile") and this writes
                    // a bogus `llamafile/{model}` assignment pointing at a
                    // catalog row that doesn't exist — clearing it here is
                    // the same treatment the empty-model_name case already
                    // gets, for the same reason: no real catalog row to
                    // assign.
                    if model_name.is_empty() || (*role == "chat" && *provider == "mesh") {
                        let _ = repo.clear_assignment(role).await;
                        continue;
                    }
                    // Derive the catalog category. Keyed on the ROLE first,
                    // because only the LLM roles have a provider to consult —
                    // ASR and TTS pass an empty provider string. The LLM arm
                    // delegates to `ModelCategory::for_chat_provider`, which is
                    // the same mapping the context governor's rung-3 lookup
                    // uses; the two used to be written out separately and a
                    // divergence would mean one of them silently addressing a
                    // row that does not exist.
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

    // A provider or model change makes the engine's warmed prefix stale, so
    // re-run the warm-up in the background. Fire-and-forget: the save must not
    // wait on a model load.
    if current.chat_provider != merged.chat_provider || current.chat_model != merged.chat_model {
        crate::spawn_prefix_prewarm(state.clone(), false);
    }

    // Return the full merged Settings so the frontend can sync its local state
    // without a second GET request.
    //
    // The echo is NOT byte-identical to what the client sent for a float field.
    // `Settings` stores these as `f32` and serde_json serialises through `f64`,
    // so a sent `0.7` comes back as 0.699999988079071. A client that diffs its
    // local state against this response must fold in the patch it sent, not
    // just the echo, or the field never converges and every later save re-sends
    // it — see docs/developer/settings-defaults-and-user-intent.md. Do not
    // "fix" this by rounding here: the widening is faithful to the stored f32,
    // and rounding on the way out would report a value the server does not hold.
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

/// Current conditions + short forecast for the dashboard weather widget.
/// Returns `{"enabled": false}` when no location is configured, rather than
/// an error — the dashboard just keeps showing its placeholder in that case.
async fn get_weather(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(provider) = state.weather_provider.as_ref() else {
        return Ok(Json(json!({ "enabled": false })));
    };

    // `{e:#}` renders the whole anyhow chain, not just the outermost context.
    // With `{e}` this said "weather API request failed" and nothing else, so a
    // call the egress gate REFUSED was indistinguishable from open-meteo being
    // down -- and PAI-2's rule is that a refusal has to be actionable. The
    // reason, the mode and the host all live further down the chain.
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

use pond_core::models::ports::provider::UnavailableProvider;

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
    /// `"mesh"` → the singleton read live out of `state.mesh_provider`'s
    ///   lock (never reconstructed here — see `pond-adapters-mesh-inference`'s
    ///   docs for why more than one would break the mesh transport's single
    ///   `recv()` consumer; `state.mesh_rebuild` is the only thing allowed
    ///   to populate it, at startup or via `PUT /settings` hot-enabling), or
    ///   a provider that fails loudly if mesh isn't actually available yet.
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

                // Resolve the catalog's real on-disk filename first — guessing
                // `{model}.gguf` fails when it doesn't match the catalog alias
                // (e.g. "llama-3.2-3b" vs "Llama-3.2-3B-Instruct-Q4_K_M.gguf").
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
/// Find a model by category + name, tolerating a TTS category that names the
/// wrong engine.
///
/// The models list buckets every TTS engine under one `tts` group, so a caller
/// that reaches for the group key instead of the record's own category asks for
/// `tts_piper/af_heart` and is told the model does not exist. `"tts"` is
/// already a legacy alias for `tts_piper` in `ModelCategory::from_str`, which
/// makes this an easy mistake to make and a confusing one to read.
///
/// Only TTS categories are retried, and only against other TTS categories, so
/// this cannot make a GGUF lookup resolve to something it did not ask for.
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

/// Scans model directories for files on disk not yet in the catalog,
/// inserts them as custom entries via the model repository, and returns
/// Read a GGUF file's header, without reading the file.
///
/// The header sits at the front, so a bounded read of the opening megabyte
/// carries every key worth having even for a model of many gigabytes. Anything
/// that is not GGUF, or is truncated, simply yields `None` — this runs inside
/// a filesystem sweep over arbitrary files and must never be the reason the
/// sweep stops.
fn read_gguf_head(path: &std::path::Path) -> Option<pond_core::models::domain::gguf::GgufInfo> {
    use std::io::Read as _;
    const HEAD_BYTES: usize = 1024 * 1024;

    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; HEAD_BYTES];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    pond_core::models::domain::gguf::parse_gguf_header(&buf)
}

/// What a whisper filename admits: `ggml-base.en.bin` is the English base
/// model, `ggml-large-v3-turbo.bin` is multilingual large.
///
/// Filename parsing, which is usually the wrong move — but a ggml `.bin` has
/// no self-describing header to ask instead, and this naming is whisper.cpp's
/// own published convention rather than a guess about someone's habits.
/// Returns `(language, size)`.
fn whisper_facts_from_name(name: &str) -> (Option<String>, Option<String>) {
    let lower = name.to_lowercase();
    let size = ["large", "medium", "small", "base", "tiny"]
        .iter()
        .find(|s| lower.contains(*s))
        .map(|s| (*s).to_string());
    // ".en" marks the English-only builds; everything else whisper ships is
    // multilingual. Only claimed when the size is recognised, so an unrelated
    // .bin in the folder is not labelled a whisper model.
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
                    // `std::fs::metadata`, NOT `entry.metadata()`. The latter
                    // does not follow symlinks, and the Hugging Face cache
                    // stores every model as a snapshot symlink pointing at a
                    // blob — so the size read was the link's own few dozen
                    // bytes, which integer-divides to 0 and reached the page
                    // as "Size unknown" on exactly the models that came from
                    // Hugging Face.
                    let path = entry.path();
                    let size_mb = std::fs::metadata(&path)
                        .map(|m| m.len() / 1_048_576)
                        .unwrap_or(0);

                    // What the file says about itself. GGUF opens with a
                    // key/value header, so this is one short read rather than
                    // a load — and it is the difference between a card headed
                    // "(detected on disk)" and one that names the
                    // architecture, quantisation and context window.
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

                    // The publisher's own name for it, when the header carries
                    // one. NOT the summary: the client maps `description` to a
                    // model's display name, so putting "Gemma3 · 4.3B · Q4_K_M"
                    // there would replace the name with its own facts. The
                    // facts travel in the structured fields below, where the
                    // page can lay them out. The placeholder stays the
                    // placeholder — the client already knows to fall back to
                    // the filename when it sees it.
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
                        // Weights plus the room a run needs around them. The
                        // catalogue's own estimates are ~25% over the file
                        // size, which is the same rule applied by hand.
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
        // Voices dropped in by hand. The catalogue lists the English ones; the
        // model repo ships 50-odd, and copying a `.bin` in is the supported way
        // to get the rest.
        extras.extend(scan_dir(
            data_dir_owned.join("models").join("kokoro").join("voices"),
            ModelCategory::TtsKokoro,
            &[".bin"],
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

/// Where a downloaded model file lands, by category.
///
/// Extracted so a resume computes the same path the original download used.
/// Two copies of this match is one rename away from a resumed download writing
/// beside the partial file it was supposed to be finishing.
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
        // Must agree with the download destination, or a resumed download
        // writes somewhere other than beside its own partial file — which is
        // the exact failure this function was extracted to prevent.
        "tts_kokoro" => data_dir
            .join("models")
            .join("kokoro")
            .join("voices")
            .join(filename),
        _ => data_dir.join("models").join(filename),
    }
}

/// `POST /api/v1/models/download/control` — pause, resume or cancel a transfer.
///
/// The filename travels in the body rather than the path: model filenames carry
/// dots and slashes, and a path segment would have to be encoded at every call
/// site to survive the router.
///
/// Pause and cancel are the same stop — the transfer checks a flag between
/// chunks, which is the only moment it is not blocked inside a read. They
/// differ in what happens to the partial file: pause leaves it, so a resume
/// picks up where it stopped; cancel deletes it.
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

    // Resume is a fresh transfer of the same file. The Hugging Face cache keeps
    // a `.incomplete` alongside the blob and re-requests with a Range header
    // when it finds one, so starting again IS continuing.
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

    // Discover any files on disk not yet in the catalog — in the background.
    //
    // This used to be awaited, so every load of the Models page paid for a
    // database read, several directory walks and a round of upserts before a
    // single byte came back. That is the page's whole latency, and it is spent
    // finding files that are almost never there: the catalog already knows
    // about anything downloaded through the app.
    //
    // Detached instead, so the response returns the rows immediately and a file
    // dropped into the folder by hand shows up on the next load rather than
    // this one. `POST /models/scan` is still the way to demand it now.
    //
    // The flag stops concurrent loads from stacking scans on top of each other:
    // the page fetches this on mount and again after every download.
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
                            // Without this arm Kokoro voices fell through to
                            // `_ => false` and reported "not downloaded" even
                            // with the file on disk — so the GUI offered the
                            // download again after every successful one.
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
    // From the record actually found, not from the requested category — a
    // forgiving TTS lookup can resolve a different one, and every write below
    // keys off this id.
    let model_id = m.id.clone();

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

    // PAI-2 P6b: refuse BEFORE spawning. The transfer itself is gated too (see
    // `spawn_tracked_download`), but a refusal that only lands in a detached
    // task shows up as a failed row in the progress tracker, which is not an
    // answer to "why did nothing download". 502 is the right shape here because
    // this handler already returns `(StatusCode, Json)` and the dashboard reads
    // `error` off a non-2xx.
    pond_core::shared::services::egress::check_egress(&url).map_err(|denied| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": denied.to_string()})),
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
        // NOT models/tts/. A Kokoro voice is a style table that the engine
        // loads from its own directory; putting it beside the Piper voices
        // makes the download report success while TTS stays broken, because
        // the adapter is reading somewhere else entirely.
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
                    // PAI-2 P6b: the config sibling is its OWN hop to its OWN
                    // host and needs its OWN gate. Gating only the weights and
                    // letting the `.onnx.json` through is exactly the shape
                    // that kept the file-level guard green in the HF cache.
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
    // From the record actually found, not from the requested category — a
    // forgiving TTS lookup can resolve a different one, and every write below
    // keys off this id.
    let model_id = m.id.clone();

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
            // Must mirror the download destination above, or "delete" leaves
            // the file on disk and the row keeps coming back as installed.
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
    // From the record actually found, not from the requested category — a
    // forgiving TTS lookup can resolve a different one, and every write below
    // keys off this id.
    let model_id = record.id.clone();

    // Persist provider keys using runtime provider names (not category names).
    // GGUF category maps to the "local" provider in runtime routing.
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
            // Also write the voice the engine actually reads.
            //
            // `active_tts_model` names the catalogue row; `voice_tts_voice` is
            // what `KokoroOutput` resolves `<voice>.bin` from and what the
            // Voice settings screen shows. Writing only the former left the two
            // disagreeing — a household could activate a voice in Models and
            // still have `voice_tts_voice` holding the Piper filename from
            // before the engine swap, so the picker showed one voice and the
            // pond spoke in another.
            //
            // Piper is deliberately excluded: its `voice_tts_voice` is a
            // filename (`en_US-ryan-high.onnx`), not a catalogue name, and the
            // resolver in `voice_models.rs` accepts either — writing the name
            // here would be a third spelling for it to guess at.
            // `record.category`, not `cat`. `cat` is what the CALLER asked for,
            // and the forgiving lookup means a request for "tts" resolves a
            // `tts_kokoro` record — so keying off the request would skip this
            // for exactly the callers that need it most.
            if record.category == ModelCategory::TtsKokoro {
                let _ = settings_repo.set_key("voice_tts_voice", name.clone()).await;
            }
        }
        "embedding" => {
            let _ = settings_repo
                .set_key("active_embedding_model", name.clone())
                .await;
            // Deliberately does NOT write `embedding_provider`. It used to write
            // `provider`, which for this category is the literal "embedding" --
            // not a runtime provider name at all, as this block's own comment
            // requires. `embedding_provider` accepts "fastembed" | "gguf" |
            // "none", so "embedding" matched nothing and silently selected the
            // fastembed arm. That was invisible while fastembed was the only
            // implementation; it is not now, because activating any embedding
            // model would quietly revert a pond that was deliberately put on
            // "gguf" -- the one provider that starts on a Jetson. Which model to
            // use and which engine loads it are separate choices; this route owns
            // only the first.
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
        // NOT done here — see scripts/jetson/llama-optimization and the note in
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
             GPU budget. See scripts/jetson/llama-optimization."
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
    // PAI-2 P6b. The refusal rides the handler's EXISTING error contract
    // (HTTP 200 with an `error` string) rather than a new status code: the
    // dashboard renders that field and throws on a non-2xx, so returning 502
    // here would turn a privacy refusal into an unhandled rejection. The text
    // is the whole `EgressDenied` Display, which names the mode, the host and
    // what to change.
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
    // PAI-2 P6b — see search_gguf_models for why the refusal rides the body.
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
    // PAI-2 P6b — see search_gguf_models for why the refusal rides the body.
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

    // PAI-2 P6b: refuse here, synchronously, rather than only inside the
    // detached task. This route takes an arbitrary caller-supplied URL, so it
    // is the one place in the file where "which host" is not decided by us.
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
                // PAI-2 P6b. Both download handlers gate before they spawn, but
                // this is the chokepoint every non-HF transfer actually passes
                // through, and the HF branch above is gated inside
                // `pond_hf_cache`. A gate only at the caller is one refactor
                // away from being no gate at all.
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

    // Taken once, up front. The callback runs per chunk and must answer
    // synchronously, so it cannot take the tracker's async lock to find out
    // whether it has been asked to stop — the atomic is shared instead.
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
            // Expected, not a failure. The `.incomplete` file is still there,
            // which is what a later call resumes from — so a pause needs
            // nothing further, and a cancel is the same stop plus a delete.
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
    // Existence check first. This used to return 204 for an id that never
    // existed, which made "did I delete the right person" unanswerable.
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

    // Count BEFORE deleting. Three of the four `profile_id` foreign keys
    // cascade, so after the DELETE there is nothing left to count.
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

    // `settings.primary_profile_id` is a key-value row, not a foreign key, so
    // nothing in the database clears it. Deleting the primary member used to
    // leave an id pointing at nobody -- and the one production reader of that
    // setting silently got `None` from the lookup, so the failure was
    // invisible. Clear it here, before the delete, while it is still true.
    // Both failures here ABORT rather than fall through. `unwrap_or_default()`
    // used to hide a read failure as `primary_profile_id: None`, so the
    // comparison never matched, the clear never ran, and the delete proceeded --
    // reintroducing the exact dangling reference this code exists to prevent,
    // on the failure path, silently.
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
        // What went with them. Sessions are RELEASED, not deleted -- a
        // conversation is not solely the speaker's -- so it is reported under
        // its own name rather than folded into a "deleted" total.
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

/// Upper bound on readings returned by one sensor history query, regardless of
/// the requested `limit`, so a wide `since`/`until` window can't pull a whole
/// retention period into memory.
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
    // Reject an unrecognised `agg` rather than quietly serving raw history
    // under it, which would answer a question the caller did not ask.
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

    // Parsed before any branch runs: a malformed bound used to fall back to an
    // unbounded query, so a typo silently widened the window to all of history
    // and any aggregate was computed over the wrong range.
    let since = parse_sensor_time_param("since", params.since.as_deref())?;
    let until = parse_sensor_time_param("until", params.until.as_deref())?;

    let storage_error = |e: anyhow::Error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    };

    // Without a sensor type there is nothing to aggregate or bound: the only
    // meaningful answer is the device's recent readings across all types.
    let Some(sensor_type) = params.sensor_type.as_deref() else {
        // ...but say so, rather than serving that answer to a caller who asked
        // a different question. `agg`, `since` and `until` are parsed and
        // validated above and then have nowhere to go on this branch, so a
        // request for "the average since Tuesday" used to come back 200 with an
        // unaggregated, unbounded list of the last 20 readings. That is the
        // exact defect this endpoint's hardening set out to remove -- answering
        // a question that was not asked -- reappearing one branch above the fix.
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

    // min/max/avg: computed by the store, so a wide window never materialises
    // its rows here.
    if let Some(a) = agg.as_deref() {
        let summary = state
            .sensor_storage
            .aggregate(&device_id, sensor_type, since, until)
            .await
            .map_err(storage_error)?;
        // The aggregate's name is itself the response key, so the body is built
        // rather than written out. `null` for an empty window is now the typed
        // answer instead of a serialized infinity.
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

    // Raw history over the requested window.
    let limit = params
        .limit
        .unwrap_or(SENSOR_HISTORY_MAX_LIMIT)
        .min(SENSOR_HISTORY_MAX_LIMIT);
    // Ask for one more than we will return: a window holding exactly `limit`
    // rows is complete, and reporting it as truncated would be its own wrong
    // answer.
    let mut readings = state
        .sensor_storage
        .get_history_limited(&device_id, sensor_type, since, until, limit + 1)
        .await
        .map_err(storage_error)?;
    let truncated = readings.len() > limit;
    readings.truncate(limit);
    let list: Vec<Value> = readings.iter().map(sensor_reading_json).collect();
    // Flag a truncated series rather than letting it read as the whole window.
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

/// Parse an optional sensor time-range param, erroring on malformed input.
///
/// Keeps [`parse_sensor_datetime`]'s grammar — RFC3339 or a bare
/// `YYYY-MM-DDTHH:MM:SS` — rather than the stricter [`parse_rfc3339_param`], so
/// requests that work today do not start failing.
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

// ── PAI-2 P8a: the would-deny read surface ────────────────────────────────────

/// Cap on audit events scanned for one policy report. The count is honest about
/// hitting it (`truncated`) rather than reporting a silently capped total: a
/// would-deny number that quietly stopped counting is the same lie the
/// two-source split below exists to avoid.
const POLICY_REPORT_MAX_EVENTS: usize = 5000;

#[derive(serde::Deserialize)]
struct PolicyReportParams {
    /// Time window for the *events* half: "hour" | "day" (default) | "week".
    window: Option<String>,
}

/// `GET /api/v1/security/policy-report` — what would flipping
/// `security_policy_mode` to `enforce` actually break?
///
/// **Two sources, labelled, never summed.** `security_policy_mode` ships as
/// `audit` precisely so this question can be answered from evidence before P8b
/// flips it, and each source alone gives a wrong answer:
///
/// - `events` comes from the unified event log. Durable across restarts, and
///   the only half that can attribute a would-deny to a principal — but it is
///   pruned on a retention window and `DELETE /api/v1/activity` is a
///   user-facing "clear my activity" button, so an operator reading only this
///   after somebody tidied up would see zero and conclude the flip is safe.
/// - `process` comes from `POLICY_COUNTERS`, bumped at each decision site.
///   Survives pruning and the clear button, does not survive a restart.
///
/// A single merged number would be wrong in whichever direction the reader did
/// not check. Adding them would double-count. So both are reported with the
/// window each covers.
///
/// Zeros, never 404, on an empty window: a green test must be distinguishable
/// from an unwired route.
///
/// Registered in `protected_routes` and deliberately absent from
/// `PUBLIC_ROUTES`, so the compile-time route guards in `middleware/mod.rs`
/// enforce the token requirement rather than this handler restating it.
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

    // Every verdict gets a bucket up front, so an empty window answers with
    // explicit zeros rather than an object missing the key the reader wanted.
    let mut by_verdict: std::collections::BTreeMap<&str, u64> =
        pol::VERDICTS.iter().map(|v| (*v, 0u64)).collect();
    let mut scanned = 0usize;
    let mut unclassified = 0u64;
    let mut truncated = false;
    let mut events_available = false;

    if let Some(event_log) = state.event_log.as_ref() {
        events_available = true;
        // `EventQuery` has no action filter and no group-by, so both happen in
        // Rust over the returned rows. That is why the cap and the `truncated`
        // flag exist: the store cannot narrow to `security.audit` for us.
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
                // An audit row written before the verdict became an attribute,
                // or by something that stopped setting it. Counted separately
                // rather than folded into `allow` -- a report that quietly
                // reclassifies what it cannot read is the failure this endpoint
                // exists to prevent.
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
    // The GUI voice path enters here, one step ahead of the chat turn it will
    // produce. Interrupting consolidation now (rather than waiting for
    // `/chat/stream`) gives the pipeline time to unwind before the user's turn
    // needs the inference slot.
    state.note_user_activity().await;

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

    // In-process transcription — no external binary needed.
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

    // Forward to external whisper.cpp /inference
    let whisper_url = format!("{}/inference", state.whisper_url);
    // PAI-2 P6b. This host is NOT a loopback literal -- it is
    // `settings.voice_whisper_url`, which defaults to 127.0.0.1:9000 but is a
    // free-text setting, so a pond configured with a remote whisper box posts
    // raw household audio to a third party. `check_egress` and not `begin`:
    // under the shipped default this fires on every transcription and would
    // otherwise write an `Internal` event per utterance, which is how a privacy
    // feed becomes something nobody reads. Loopback passes every mode.
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
    // In-process first, exactly as POST /transcribe does. Without this branch
    // the handler went straight to the HTTP whisper server, which a default
    // build never spawns — that spawn is `#[cfg(feature = "legacy-subprocess")]`
    // — so onboarding's wake-word calibration step returned 502 on every
    // install. The CLI `pond-server calibrate` path did work in-process, so the
    // two calibration routes disagreed and only one of them functioned.
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
        // PAI-2 P6b -- same configurable-host reasoning as `transcribe_audio`.
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
///
/// Gated (PAI-2 P6b) even though two of its three call sites are 127.0.0.1
/// literals: the third is `state.whisper_url`, which the user can point
/// anywhere. `check_egress` permits loopback under every mode, so the
/// diagnostics page is unchanged on a default install and reports a refusal --
/// with the setting to change -- only for a destination the mode really refuses.
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
    /// Fire once at `cron`'s next occurrence instead of recurring — see
    /// [`pond_core::user_data::ports::scheduler::CreateScheduleRequest::once`].
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

    // A rule can still arrive here, by naming the kind explicitly — this route
    // predates `/rules` and clients depend on it. Validate it identically or
    // the new surface's checks are decorative: whoever wanted to skip them
    // would simply POST here instead. (PAI-7 P8)
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
    /// Fire ONCE at this instant instead of recurring — see
    /// [`pond_core::user_data::ports::scheduler::UpdateScheduleRequest::fire_at`].
    #[serde(default)]
    fire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Fire once at `cron`'s next occurrence instead of recurring — see
    /// [`pond_core::user_data::ports::scheduler::UpdateScheduleRequest::once`].
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

// ───────────────────────── Sensor-rule Handlers (PAI-7 P8) ──────────
//
// A rule is a `TaskKind::SensorTrigger` schedule and nothing else — there is no
// second store and no second execution path, because the rules engine fires
// through the scheduler's own `run_now` and that is what gives a rule fire its
// run record and its result event. What this surface adds is the two things
// `/schedules` cannot: it names rules (so listing them does not mean filtering
// a mixed list client-side, and creating one does not mean inventing a cron
// expression the scheduler ignores), and it validates the spec.

/// Project a schedule onto the rule surface. `None` when the id names a cron
/// schedule rather than a rule.
///
/// Every `/rules/{id}` handler resolves through this, which is what stops the
/// new surface being a second door onto `/schedules`: pausing or deleting the
/// household's nightly backup by guessing its id answers 404 here.
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
        // PAI-7 P8's durable cooldown stamp. Exposed because "why has this not
        // fired" is answered by it and by nothing else on the response.
        "last_fired": s.last_run,
        "cooldown_secs": spec.cooldown_secs,
        "source": spec.source,
        "condition": spec.condition,
        "actions": spec.actions,
    }))
}

/// Rules never register a cron job — the rules engine fires them from the event
/// bus — but a `Schedule` has to carry some cron string. This is the marker the
/// scheduler and the MCP tools already use, kept here so a caller never has to
/// supply an expression that is not read.
const RULE_CRON: &str = "@event";

/// The 400 a rule spec earns, or `None` if it is fit to store.
///
/// One function for all three doors — `POST /rules`, `PUT /rules/{id}` and the
/// older `POST /schedules` — because a rule accepted through any of them is
/// stored in the same place and fired by the same engine. A check on two of the
/// three is not a check.
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
        // `rule_view` decides what a rule is, here as well as in the listing.
        // Two encodings of "is this a rule" drift, and the direction this one
        // would drift in is a cron schedule becoming reachable by id.
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

/// Create/replace body. The spec is flattened, so the rule reads as one object
/// rather than a schedule wrapping a kind wrapping a spec.
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

    // Duplicate ids and the cron are the scheduler's to judge, not this
    // handler's — see `SensorTriggerSpec::validate`.
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
    // Resolve BEFORE validating so a PUT at a cron schedule's id is a 404
    // rather than a 400 telling the caller how to fix a rule that does not
    // exist -- and so it can never overwrite one.
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
/// Returns a flat array of tool objects: `{ "extension": "...", "name": "...",
/// "description": "..." | null }`. Tools with associated MCP App resources
/// also include `{ "_meta": { "ui": { "resourceUri": "ui://..." } } }`.
/// Returns an empty array when no extension manager is active (no-crash fallback).
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
                    // Inject _meta.ui for tools that have an associated MCP App
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

/// The tools reachable without an LLM turn, through `POST /api/v1/tools/invoke`
/// and `POST /api/v1/mcp/tools/call`. Deny-by-default.
///
/// These routes carry no engine session, so the `_meta` the MCP servers read the
/// caller from is empty: `session_from_meta` returns `None` and every policy
/// decision behind them resolves to an unknown actor. An unknown actor is not a
/// denied one — `PolicyDecision::refuse` sets `allowed = !mode.denies_bite()`, so
/// under `PolicyMode::Audit` it is let through. A tool whose guard asks *who is
/// calling* therefore has no guard at all on this path, and
/// `giap-draft__approve_draft` is precisely that tool: it is the human
/// confirmation step for every staged side effect.
///
/// So the surface stays as small as what the shipped UI actually needs. The Hub
/// actuates devices from `hubStore.ts` and `Rooms.tsx`; MCP Apps proxy through
/// `callServerTool`, and today there is exactly one app — the weather card —
/// which makes no tool calls of its own. Everything else goes through a chat
/// turn, where the engine stamps a session into `_meta` and the ownership check
/// can actually run.
///
/// Adding a name here grants it to any paired client *and* to any sandboxed MCP
/// App iframe. Read-only or narrowly-actuating tools only; nothing that decides,
/// approves, executes shell, writes files, or reads household memory.
const DIRECT_DISPATCH_ALLOWLIST: &[&str] = &[
    "giap-device-control__set_device_state",
    // Read-only, and the counterpart to the line above. The desktop could
    // actuate a device but not ask it anything, so the Devices card labelled its
    // power button from `is_online` -- reachability, a different fact -- and
    // offered "Turn on" to a contact sensor. See `powerStateOf` in
    // `pond-desktop/src/sections/Devices.tsx`.
    "giap-device-control__get_device_state",
    "giap-weather__get_current_weather",
    "giap-weather__get_weather_forecast",
];

/// Shared direct-dispatch path for the tool-invoke endpoints. Bypasses the LLM:
/// routes straight to the MCP tool registry and returns the raw result.
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
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use futures::stream::StreamExt;
    use pond_core::shared::domain::agent::AgentRequest;

    // PAI-1 P9. This route hand-parses a raw `Value` body, which is exactly the
    // shape where a `body["device_id"]` would be one line away from looking
    // reasonable. It is read from the principal, before the body is touched.
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

    // Phase F1 parity: this route hand-parses its body (it takes a raw Value,
    // not a typed DTO), so `images` has to be pulled out explicitly. A malformed
    // `images` field is ignored rather than fatal, matching how every other
    // optional field on this route behaves.
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

        // PAI-5 P6. This route reads no settings otherwise, so the one read is
        // here and it narrows: an unreadable settings row means
        // `persist_thinking` stays false and the reasoning is not kept. A
        // privacy control that defaults ON when its store is unavailable is the
        // scope-widening default this programme treats as a bug.
        let persist_thinking = state
            .settings_repo
            .get()
            .await
            .map(|s| s.persist_thinking)
            .unwrap_or(false);

        // The session row first, because both things below depend on it: the
        // scope resolution reads the session's identity, and the user message is
        // a row against it.
        if storage.get_session(&session_id).await.is_err() {
            if let Err(e) = storage.create_session(session_id.clone()).await {
                yield Ok(Event::default().data(json!({"error": e.to_string()}).to_string()));
                return;
            }
        }

        // Resolved here, not before the stream: this route creates the session
        // row inside the stream body, so resolving earlier would read the
        // identity of a session that does not exist yet and miss a binding
        // written by PUT /sessions/{id}/user against a freshly minted id.
        //
        // PAI-5 P7 moved it ABOVE the `ChatService` rather than below, and that
        // ordering is the whole safety of this phase. `ChatService`'s default
        // scope is `ProfileScope::Household`, and the scope is what memory
        // extraction is attributed to. Enabling extraction here -- which is
        // exactly what P7 asks for -- while building the service before the
        // scope was known would have written a Guest's turn, or one member's,
        // into the whole household's memory. The default was harmless only
        // because extraction was off on this route. A widening default reached
        // by ordering is still a widening default.
        let turn_scope = resolve_turn_scope(&state, &session_id, &device).await;

        let mut chat_service = pond_core::shared::services::chat::ChatService::new(
            agent.clone(),
            session_id.clone(),
            storage.clone(),
        )
        .with_profile_scope(turn_scope.clone())
        .with_thinking(persist_thinking);
        // PAI-5 P7 parity. `/chat/stream` has owned extraction since it was
        // written; this route persisted its turns and never extracted from them,
        // so a whole conversation held here contributed nothing to memory.
        // Guarded exactly as the other handler guards it, so a pond with no
        // extractor configured behaves as it did before.
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

        // `message` is moved into the `AgentRequest` below, and extraction needs
        // the user's own words when the turn ends.
        let user_message_for_extraction = message.clone();

        // The same accumulator `/chat/stream` uses, so both routes fold an
        // engine event into a turn the one way. This route never captures
        // thinking blocks out of the token stream -- it takes the engine's
        // structured `Thinking` events and nothing else.
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
                            // PAI-5 P6, same contract as `chat_stream`: offered
                            // unconditionally, kept only if the user said so.
                            chat_service.record_thinking(block);
                            yield Ok::<Event, std::convert::Infallible>(Event::default().data(frame));
                        }
                        // This route closes on the engine's `Done`, which is why
                        // the `done` frame is built here rather than in the
                        // translator -- `/chat/stream` keeps the numbers and
                        // emits its `done` after persistence.
                        //
                        // PAI-5 P2: the turn's stats go out FIRST, through the
                        // same builder the other route uses. This route used to
                        // drop them on the floor with a `{ .. }`, so a client
                        // driving `/agent/chat/stream` could see no TTFT, no
                        // decode rate, no context fill and no reasoning count
                        // for a turn the engine had measured. `usage` is still
                        // dropped: this route reports no totals in its `done`
                        // frame, and inventing one now would change a shape
                        // clients already parse.
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
                    // Emit thinking blocks captured by the filter
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
        // PAI-5 P7. `persist_assistant_turn_with_extraction` owns both concerns,
        // which is why this route calls it rather than persisting and then
        // extracting: a handler cannot accidentally drop extraction by
        // refactoring the block, because there is no separate call to drop.
        // That is the same reason `/chat/stream` uses it.
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
///
/// Returns up to 10,000 rows across all severity levels.
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
/// Restarts a marketplace extension so its child process picks up whatever
/// credentials are currently in the secret store.
///
/// A stdio extension reads its credentials from the environment it was spawned
/// with, so a credential change has no effect at all until the process is
/// replaced. Every caller here has just changed one.
///
/// Returns `Err` with a reason whenever the restart could not be carried out,
/// so no caller can report success over an extension that is not running. The
/// three collaborators being absent is one of those reasons rather than a
/// silent no-op: on a backend that has no extension manager the credentials
/// are stored and nothing is listening for them, which the user has to be told.
///
/// This restarts whatever it is asked to, including an extension that is not
/// installed yet — the install flow signs in before it installs, so refusing
/// would break it. Deciding whether an extension *should* be started is the
/// caller's policy; see `set_extension_secrets_handler`.
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

    // Best-effort: this fails routinely and harmlessly when the extension was
    // not running, which is the normal case on a fresh install.
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

    // Storing is only half the job: a stdio extension reads its credentials
    // from the environment of the process it was spawned in, so until it is
    // restarted the new values change nothing. Both outcomes are reported
    // because the store has already succeeded — the caller needs to know the
    // secrets are safe AND whether anything is actually using them yet.
    // Applying credentials is only ever meant to fix something the user
    // already runs. Starting an extension they never installed, or one they
    // deliberately disabled, would be a side effect nobody asked for — this
    // endpoint is reachable independently of the install flow.
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

/// Secret-store key for a per-provider OAuth client ID override, e.g.
/// `"SPOTIFY_CLIENT_ID"`. Always call this with the canonical `provider.id`
/// (never a raw request field, which may be a secret's token_key instead) —
/// every handler that resolves a client ID must agree on this key or the
/// authorize and token-exchange steps end up using different apps.
fn client_id_secret_key(provider_id: &str) -> String {
    format!("{}_CLIENT_ID", provider_id.to_uppercase())
}

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

    // Check if user has their own client ID in the secret store.
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
/// Escapes text that is interpolated into the OAuth result pages.
///
/// Those pages embed strings GIAP does not control — a subprocess's stderr, or
/// an error body returned by the OAuth provider — so they must not be able to
/// close a tag and inject markup into a page rendered on 127.0.0.1.
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

    // Every terminal branch below records how the flow ended, so the UI that
    // started it can poll for the real answer instead of guessing from whether
    // a token key happens to exist.
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

    // Find provider config
    let providers = pond_core::user_data::services::oauth_providers::builtin_oauth_providers();
    let provider = match providers.iter().find(|p| p.id == session.provider_id) {
        Some(p) => p,
        None => {
            fail("Unknown OAuth provider.").await;
            return Html("<h1>Authorization failed</h1><p>Unknown provider.</p>".to_string())
                .into_response();
        }
    };

    // Resolve client ID (user override or bundled)
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

    // Exchange authorization code for tokens.
    // The redirect_uri MUST exactly match the one sent in the authorize request.
    let redirect_uri = format!("http://127.0.0.1:{}/api/v1/oauth/callback", state.api_port);
    // PAI-2 P6b. The code exchange is a POST of a live credential to a third
    // party, and it was ungated -- `authorization_code` as well as the refresh
    // below. The refusal goes through `fail` so the UI polling
    // `/oauth/status/{state}` gets the reason instead of a hang.
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
            let restart_error = match &session.extension_id {
                Some(ext_id) => restart_extension_with_secrets(&state, ext_id).await.err(),
                None => None,
            };

            // The tokens are stored either way, but if the extension could not be
            // started there is nothing working on the other side — say so rather
            // than showing a green "Connected" card over a dead extension.
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
///
/// Returns `pending` while the browser hand-off is still in flight, then
/// `completed` or `failed` once the callback has run. `unknown` means the
/// nonce was never issued by this process, or its outcome has aged out.
///
/// This exists so the sign-in UI can wait for the flow it actually started.
/// Watching the secret store instead reports success the moment a token key is
/// present — which, when re-authorising, is true before the user has done
/// anything at all.
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
        let key = client_id_secret_key(&provider.id);
        repo.get(&key)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| provider.bundled_client_id.clone())
    };

    // PAI-2 P6b: the refresh hop, gated separately from the code exchange.
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
        .get(&client_id_secret_key(&provider.id))
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| provider.bundled_client_id.clone());

    // PAI-2 P6b: hop 1 of 3 on this path. The other two are in
    // `spotify_api_call`, and each one is gated where it is made -- a single
    // gate at the top of the flow is the shape that lets a copy-pasted retry
    // out through a hole nobody can see.
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

/// The egress tracker records the method as a `&'static str`, deliberately: an
/// attacker-influenced method string must never become an unbounded attribute
/// key in the event store. `reqwest::Method` is not `'static`, so map it.
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

/// Why a Spotify call produced no response.
///
/// PAI-2 P6b replaced a bare `Option` here. Folding a network-mode refusal into
/// the same `None` that means "Spotify was never connected" makes the dashboard
/// tell the user to sign in again, which cannot work and does not name the
/// setting that is actually stopping the call. A refusal nobody can act on is
/// the failure PAI-2 invariant 1 exists to prevent.
enum SpotifyUnavailable {
    /// No secret storage, or no stored token: Spotify was never connected.
    NotConnected,
    /// The network mode refused a hop. Carries the whole `EgressDenied` text.
    Refused(String),
}

/// Calls the Spotify Web API with the stored access token, transparently
/// refreshing and retrying once on a 401.
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

    // PAI-2 P6b, hop 2 of 3.
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
    // PAI-2 P6b, hop 3 of 3. The retry is its own request to its own host and
    // carries a freshly minted credential; it gets its own gate. Reusing the
    // gate above would be the copy-paste regression this file's guard is built
    // to catch.
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

/// Maps a failing Spotify Web API status onto a stable machine-readable code
/// and text the dashboard can show the user verbatim.
///
/// The 403 wording is the one that matters: Spotify apps in development mode
/// only serve accounts explicitly allowlisted in the developer dashboard, and
/// a non-allowlisted account still completes the whole OAuth flow — consent,
/// code exchange, refresh token — before every single API call fails. Without
/// naming that, the failure is indistinguishable from a paused player.
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
        // PAI-2 P6b: not the same thing as "not connected". The widget can say
        // which setting to change instead of offering a sign-in that will not
        // help.
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
        // Everything else is a real failure. Reporting these as an idle player
        // made a connection Spotify was actively refusing look like a paused
        // one, leaving the widget with nothing to tell the user.
        let (error, message) = spotify_error_hint(status);
        let body_preview = resp.text().await.unwrap_or_default();
        tracing::warn!(status = %status, error, body = %body_preview, "Spotify now-playing request failed");
        return Json(json!({
            "connected": true,
            "playing": false,
            "error": error,
            "message": message,
            // The literal upstream status, not just the code derived from it.
            // The widget stops polling after a run of 4XX answers, and only a
            // 4XX may count: `unavailable` covers 5xx too, and a Spotify
            // outage — or this pond restarting — must not permanently silence
            // a widget whose recovery needs somebody to press something.
            "upstream_status": status.as_u16(),
        }))
        .into_response();
    }

    let mut body: serde_json::Value = resp.json().await.unwrap_or_default();

    // Spotify's `/me/player/currently-playing` frequently omits `item` for
    // podcast episodes even though `currently_playing_type` correctly says
    // "episode" — a known gap in Spotify's own API, not something this
    // request got wrong. `/me/player` (the fuller playback-state endpoint)
    // sometimes has what the leaner one didn't, so it is worth one extra
    // call, but only in the case that actually needs it.
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

/// Shape a Spotify `/me/player/currently-playing` (or `/me/player`) body into
/// the JSON the dashboard widget expects. Pure and synchronous on purpose —
/// everything network- and auth-shaped already happened in the caller, so
/// this is the part that can be pinned with plain fixtures instead of a
/// mocked `api.spotify.com`.
fn now_playing_snapshot(body: &serde_json::Value) -> serde_json::Value {
    let item = &body["item"];

    // A track and a podcast episode are shaped differently: an episode has
    // no `artists` array (so the track-shaped read below silently landed on
    // "" for every field that mattered) and carries its own `images` rather
    // than nesting them under `album`. Spotify names which shape `item` is
    // in via `currently_playing_type`, so branch on that instead of assuming
    // every playing thing is a song.
    let playing_type = body["currently_playing_type"].as_str().unwrap_or("");
    let (track, artist, album_art, duration_ms) = match playing_type {
        "episode" => (
            item["name"].as_str().unwrap_or(""),
            item["show"]["name"].as_str().unwrap_or(""),
            item["images"][0]["url"].as_str(),
            item["duration_ms"].as_i64().unwrap_or(0),
        ),
        // "track", "ad", "unknown", or absent. An ad or an unknown type may
        // still carry a track-shaped `item` (or none at all, in which case
        // every `.as_str()`/`.as_i64()` below is the existing empty-default
        // behavior — unchanged for that case).
        _ => (
            item["name"].as_str().unwrap_or(""),
            item["artists"][0]["name"].as_str().unwrap_or(""),
            item["album"]["images"][0]["url"].as_str(),
            item["duration_ms"].as_i64().unwrap_or(0),
        ),
    };

    // Spotify sometimes never populates `item` at all for a playing episode
    // or ad, on either endpoint the caller tries — a gap on Spotify's side,
    // with no further in-band workaround. An empty `track` here reads as
    // "the widget is broken"; naming what IS known (that something is
    // playing, and roughly what kind) is honest instead.
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
        // PAI-2 P6b.
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
        // Same reasoning as the now-playing handler: say which failure it is
        // rather than reporting an authorisation problem as a generic outage.
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
    /// Absent means "leave it alone", not "clear it".
    ///
    /// It was `#[serde(default)] String`, so an omitted field arrived as `""`
    /// and was written straight over the stored value. The desktop client sends
    /// `{ content }` and nothing else, so every save from the Prompts tab wiped
    /// the description of the template it was editing.
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
    // Preserve the built-in flag of an existing row — editing "balanced" must
    // not strip its system status (deletion protection) — and mark the row
    // customized so the startup factory reseed leaves the edit alone.
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
        // The generation this edit was FORKED FROM, carried through unchanged.
        // Stamping the current one here would mark every save as up to date and
        // permanently suppress the notice that a newer built-in exists — the
        // user's edit is based on whatever they were looking at when they
        // opened the editor, which is what this number records.
        factory_version: existing.as_ref().map(|t| t.factory_version).unwrap_or(0),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    match repo.upsert(&template).await {
        // The SAVED ROW, not a status stub.
        //
        // This returned `{"name","status":"ok"}` while the desktop client typed
        // it `Promise<PromptTemplate>` and then did `setBodies(… updated.content)`.
        // `updated.content` was `undefined`, so a SUCCESSFUL save blanked the
        // editor — and the natural response to that is to hit Reset, which hands
        // the row back to the factory. The bug quietly undid the edits it was
        // reporting success for.
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
        is_customized: false,
        // A reset takes the CURRENT built-in, so the row is current by
        // definition and the "newer version available" notice must clear with
        // it. Leaving the old number here would leave the notice up forever on
        // a row that just adopted.
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

#[derive(Deserialize)]
struct UpdateMemoryRequest {
    content: String,
}

/// `PUT /api/v1/memories/{id}` -- correct a memory's wording in place.
///
/// In place, keeping its id. The desktop used to do this by adding the new text
/// and deleting the old row, which reset the memory's age and usage, orphaned
/// its vector, and left a duplicate behind whenever the delete half failed. A
/// correction should not turn a long-held fact into a brand-new one.
async fn update_memory(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateMemoryRequest>,
) -> impl axum::response::IntoResponse {
    let content = body.content.trim();
    if content.is_empty() {
        // Emptying a memory is a deletion wearing an edit's clothes, and the
        // caller has a route for that which reports what it removed.
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
                Json(json!({"error": "Memory consolidation is not available in this build"})),
            )
                .into_response();
        }
    };

    // Read the CURRENT setting, not a startup snapshot — flipping the toggle in
    // Settings must take effect without restarting the server.
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

/// A recipe extension entry, as goose's own `extensions:` shape
/// (`{type, name, timeout?, bundled?}`).
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

/// The fields GIAP reads out of a recipe's YAML.
///
/// `deny_unknown_fields` is deliberately NOT set: a recipe may legally carry
/// anything Goose understands, and refusing to run it because we do not read a
/// field would be worse than ignoring it. What we do instead is say so — see
/// [`RecipeYaml::warn_about_dropped_fields`].
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
    /// Present only so the warning below can notice them — sub-recipe
    /// composition and structured response schemas are out of scope for
    /// GIAP's recipe runner; both parse cleanly and are silently dropped.
    #[serde(default)]
    sub_recipes: Option<Value>,
    #[serde(default)]
    response: Option<Value>,
}

impl RecipeYaml {
    fn parse(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Recipes carrying `sub_recipes` or a structured `response` schema parse
    /// cleanly and then run as a bare prompt with those fields silently
    /// dropped — GIAP delegates to the ordinary chat-stream pipeline, which
    /// has no sub-recipe execution and no structured-output enforcement.
    /// Silence here reads as support.
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

/// Maps a recipe's `extensions[].name` (goose's own extension vocabulary) to
/// GIAP tool-group prefixes. Names with no GIAP equivalent are dropped, not
/// refused — an `extensions:` list is a narrowing request, never a widening
/// one, so an unrecognised name simply grants nothing rather than erroring.
fn recipe_extension_to_tool_group(name: &str) -> Option<&'static str> {
    match name {
        "weather" => Some("giap-weather"),
        "schedule" | "scheduler" => Some("giap-schedule"),
        "memory" => Some("giap-memory"),
        "device" | "developer" => Some("giap-device"),
        // Home actuation is `giap-device-control`. This said `giap-matter` for
        // as long as the mapping has existed, and `giap-matter` is the name of
        // the matter.js WEBSOCKET PROTOCOL (`pond-adapters-matter`), never a
        // tool group -- so a `home` recipe narrowed to a group no tool belongs
        // to and ran with no tools at all.
        "matter" | "home" => Some("giap-device-control"),
        _ => None,
    }
}

/// Merge a recipe's parsed YAML fields into its JSON representation, so list
/// and write responses surface `title`/`parameters`/`extensions`/`activities`
/// alongside the raw `yaml`. A recipe with YAML GIAP cannot parse still lists
/// — it degrades to empty arrays rather than breaking the response.
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

/// Substitute `{{key}}` placeholders in `text` with values from `values`.
/// Plain string replacement, matching goose's own `{{key}}` recipe syntax —
/// there is no conditional/loop logic in a recipe prompt, so a templating
/// engine would be pulling in machinery to do what `str::replace` already does.
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

/// Execute a recipe by name. Looks up the AgentRecipe, parses its YAML,
/// validates and substitutes `parameters`, resolves `extensions` to a
/// tool-group allowlist, and delegates to the shared chat-stream pipeline so
/// the response matches `POST /api/v1/chat/stream` event-for-event.
async fn run_recipe(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    body: Option<Json<RunRecipeRequest>>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)>
{
    // PAI-1 P9. A recipe run is a turn like any other and resolves the speaker
    // the same way: the device the pond issued this caller's token to, never
    // anything in `RunRecipeRequest`.
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

    // Validate required parameters before touching the chat pipeline — a
    // half-substituted prompt is worse than a 400.
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

    // Effective values: declared defaults, overridden by whatever the caller
    // supplied.
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

    // `extensions:` is a narrowing request: unrecognised names are dropped
    // with a warning rather than refusing the run.
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
        // A recipe run is driven by the schedule, not by a client that might
        // come back for it.
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

// ── Proactive proposals (PAI-7 P3b) ──────────────────────────────────────────
//
// P3a landed the `Proposal` domain, `ProposalRepository`, `SqliteProposalRepository`
// and migrations 0041/0042, and said in its own stamp that NOTHING constructed
// any of it. This is the surface that lets a person see and dispose of one.
//
// # Reusing the drafts machinery is the design decision, not an implementation
// # shortcut
//
// A proposal IS a `drafts` row (`origin = 'proactive'`), so disposing of one is
// `DraftRepository::update_status` and the ownership question is
// `is_draft_decision_permitted` -- the same function `giap-draft`'s
// `DraftMcpServer::decide` asks. That means a proactive suggestion inherits an
// approval flow that already exists rather than growing a second one, and a
// later change to the ownership rule reaches both callers.
//
// **Approving does not execute anything.** It moves the row to `approved`,
// exactly as `DraftMcpServer::apply` does. Invariant 1 is "GIAP proposes; the
// user disposes", and the executor is PAI-7 P4's business.
//
// # Why the decide route reads the PROPOSAL repository and not the draft one
//
// `DraftRepository::get` would answer for any draft id, and this route would
// then be a second way to decide a user-staged draft -- one that skips the
// policy tally and the audit entry `DraftMcpServer::decide` records, which is
// the telemetry PAI-2 P8b's enforce flip is waiting on. `get_live` answers only
// for a row that is proactive, pending and unexpired, so an id that is anything
// else is a 404 here and stays the MCP path's business.
//
// The cost of that is real and deliberate: rejecting an EXPIRED proposal is not
// possible at this edge, though section 3.5 wants rejections as feedback. The
// MCP path still allows it (`decide` gates only approval on liveness). Rather
// than widen this route to every draft to get it, the honest fix is a
// `ProposalRepository` read that returns a decided-or-expired proposal, which
// belongs with the phase that builds the feedback loop.

/// The caller of a proposal route, as one member.
///
/// Invariants 4 and 5 at the HTTP edge, answered by the domain's own door
/// rather than by a check written here. [`ProposalAudience::from_scope`] admits
/// `Owner(id)` and refuses the other two, for opposite reasons that both land
/// on "not one member": `Guest` generates and receives nothing, and `Household`
/// is not a weaker address than `Owner` — it *is* the broadcast.
///
/// **The consequence is a real cliff and I am taking it deliberately.** A
/// session that nobody has identified resolves to `Household` on a one-member
/// pond, so on a default install today this answers 403 and the surface is
/// unusable until the speaker is resolved to a member — by
/// `PUT /sessions/{id}/user`, by a face match, or (since PAI-1 P9's identity
/// half) by the request arriving on a device paired with a member-bound code.
/// The third of those needs no session binding at all: `resolve_turn_scope`
/// resolves it per request, from the token. That is narrower than
/// [`is_draft_decision_permitted`](pond_core::security::ports::policy::is_draft_decision_permitted),
/// which lets `Household` decide any draft on the argument that a one-member
/// pond has nobody to protect from. The difference is that a draft can be
/// unowned and a proposal never is: to LIST one I would have to pick a member,
/// and picking is the fallback PAI-1 P3 refused. Rather than let the read refuse
/// while the write permits — you could then dispose of what you cannot see — both
/// go through this one door.
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

/// PAI-7 P3a's repository, built from the pool `AppState` already holds.
///
/// It belongs on `AppState` as an injected `Arc<dyn ProposalRepository>`, next
/// to `session_storage` and the rest, and it is not there because `lib.rs` was
/// owned by other work this round. Everything that needs it goes through this
/// one function precisely so the swap is a one-line change here and no change
/// in any handler.
fn proposal_repo(state: &Arc<AppState>) -> pond_infra::sqlite_proposal::SqliteProposalRepository {
    pond_infra::sqlite_proposal::SqliteProposalRepository::new(state.db.system.clone())
}

/// The drafts repository, same story as [`proposal_repo`]. A proposal is a
/// `drafts` row, and this is what moves it to `approved` or `rejected`.
fn draft_repo(state: &Arc<AppState>) -> pond_infra::sqlite_draft::SqliteDraftRepository {
    pond_infra::sqlite_draft::SqliteDraftRepository::new(state.db.system.clone())
}

/// The wire shape of a proposal.
///
/// Built field by field from the accessors rather than by serialising the type,
/// so the JSON is a decision rather than a consequence of the struct layout —
/// and so `summary` (which is a method, not a field) is in it. Invariant 2's
/// `rationale` is a top-level key beside it and never folded into the summary:
/// a client that renders only the summary must not be able to look like it is
/// showing the reason.
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

/// Which session is asking. Required, and deliberately not defaulted: the
/// session is how this edge learns who the caller is, and a defaulted one would
/// resolve to `Household` — the broadcast — for every caller.
#[derive(Deserialize)]
struct ProposalCallerQuery {
    session_id: String,
}

/// `GET /api/v1/proposals?session_id=X` — the live proposals addressed to the
/// member this session belongs to, newest first.
///
/// There is no "list every proposal" route and there will not be one: the port
/// has no method for it, because the only caller that could want one is a
/// broadcast (invariant 4). Expiry is filtered in SQL on every read, so a stale
/// suggestion cannot be listed even though nothing sweeps the table
/// (invariant 7).
async fn list_proposals(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    Query(query): Query<ProposalCallerQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // PAI-1 P9. `ProposalCallerQuery` carries the session id and nothing else;
    // the device comes from the principal, so a query string cannot widen who
    // this caller is addressed as.
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

/// Approve or reject. There is no third value and no default: a decision this
/// route could not read is a 400, never an approval.
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

/// `POST /api/v1/proposals/{id}/decide` — the member disposes.
///
/// Approving moves the row to `approved` and executes nothing; see the section
/// header above.
async fn decide_proposal(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    principal: Option<axum::Extension<pond_core::security::ports::policy::Principal>>,
    body: Result<Json<DecideProposalRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // PAI-1 P9. Read before the body: `DecideProposalRequest` is
    // `deny_unknown_fields`, so a `device_id` in the body is a 400 rather than
    // an identification, and this is where the real one comes from anyway.
    let device = proven_device(principal.as_ref());
    let Json(request) = body.map_err(|_| bad_body())?;
    let (scope, _audience) = proposal_caller(&state, &request.session_id, &device).await?;

    // Only a live proposal is decidable here, and `get_live` is what decides
    // that this id is a proposal at all. Expired, already decided, a
    // user-staged draft and absent are one answer on purpose.
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

    // The shared ownership rule, asked with the proposal's own owner and the
    // sentinel session `SqliteProposalRepository::save` writes. `Owner(id)` may
    // decide only its own; a different member gets `REASON_FOREIGN_DRAFT`.
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
            // Migration 0041's BEFORE UPDATE trigger is the layer under this
            // one and it aborts an approval it does not like. That is a refused
            // transition, not a broken pond.
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

/// Work out whose turn this is, once, at the edge.
///
/// PAI-1 P3. Called before the agent stream starts so the verdict can ride
/// `AgentRequest` down rather than be recomputed nearer the data, where a
/// second resolution could disagree with the first and the deeper one would
/// silently win.
///
/// Every failure here narrows rather than widens. A session-storage error
/// resolves as if nothing were bound, a profile-list error is treated as "there
/// may be more than one member", and an unreadable attribution store identifies
/// nobody -- all three give the more restrictive answer, per invariant 2.
///
/// # The device argument
///
/// PAI-1 P9's identity half. `device` is a [`ProvenDevice`], not a `&str`, and
/// that is the whole security property: the only way to build one that names a
/// device is [`ProvenDevice::from_principal`], and `Principal::device_id` is
/// populated in exactly one place -- the auth middleware, from
/// `Handshake::caller_for_token`, which reads the token this pond issued at
/// pairing. `IdentificationSource::PairedDevice` outranks face and explicit
/// identification, so a device id a client could supply in a header or a body
/// would outrank every proof the pond can make. There is no constructor that
/// takes one.
///
/// An unattributed device (`devices.profile_id` NULL) falls THROUGH to the next
/// rung and resolves nobody -- migration 0043's header is explicit that the
/// identity and delivery directions are not mirror images, and this is the
/// identity one.
async fn resolve_turn_scope(
    state: &Arc<AppState>,
    session_id: &str,
    device: &ProvenDevice,
) -> ProfileScope {
    // The strongest rung first, and the read only happens when the request
    // actually carried a device. `rung` consumes the `Result` so a failed read
    // cannot be flattened into "unattributed" by an `unwrap_or_default`.
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
        // PAI-1 P9. `Member` is the only one of the four rungs that answers
        // `Some`; no device, an unattributed device and an unreadable
        // attribution store all answer `None` and fall through.
        paired_device_profile: device_rung.profile_id(),
        session: &identity,
        household_has_multiple_members,
    })
    .scope
}

/// The device the *pond* proved this request came from.
///
/// Every handler that resolves a turn goes through this one function, so there
/// is a single place where a request becomes a device id and it reads only the
/// `Principal` the auth middleware attached. A request with no principal --
/// an in-process caller, or a wiring fault -- names no device and falls through
/// every rung below the paired-device one, which is what it did before this
/// rung existed.
fn proven_device(
    principal: Option<&axum::Extension<pond_core::security::ports::policy::Principal>>,
) -> ProvenDevice {
    match principal {
        Some(axum::Extension(principal)) => ProvenDevice::from_principal(principal),
        None => ProvenDevice::none(),
    }
}

// ── PAI-8 P1: connecting a source ──────────────────────────────────────────
//
// Without these there is no way to create a `ContextSource`, and with no source
// the ingest pipeline has nothing to ingest under -- so `context_items` stays
// empty on every pond however well the rest of it works. `upsert_source` had no
// caller at all until this.
//
// Section 3.4 lists these under "New surfaces" without assigning them a phase.
// They are P1's, because P1's own sentence -- "the ingest pipeline with on-pond
// sources only" -- presupposes that a source can exist.

/// The context repository, same story as [`proposal_repo`]: built from the pool
/// `AppState` already holds because `lib.rs` and `main.rs` were owned by other
/// work. It takes the redactor because storage redacts on the way in.
fn context_repo(state: &Arc<AppState>) -> pond_infra::sqlite_context::SqliteContextRepository {
    // `RuleRedactor` is deterministic and stateless, so constructing one here cannot disagree with
    // the one `main.rs` holds. If it ever grows configuration it belongs on `AppState` instead.
    pond_infra::sqlite_context::SqliteContextRepository::new(
        state.db.system.clone(),
        std::sync::Arc::new(pond_infra::rule_redactor::RuleRedactor::new()),
    )
}

#[derive(serde::Deserialize)]
struct ConnectSourceRequest {
    /// `sensor` or `camera` today. Anything else is refused by
    /// `SourceKind::availability`, which quotes its own sentence about what is
    /// missing.
    kind: String,
    /// The device this source follows -- a `device_id` for a sensor, a
    /// `camera_id` for a camera. It is matched against the bus event, so a
    /// value naming no device produces a source that never ingests anything.
    provider: String,
    /// Which conversation the caller is speaking in. The OWNER is resolved from
    /// this and the caller's proven device; see below.
    session_id: String,
    /// Sign-in details, for a kind that reaches an account.
    ///
    /// REQUIRED for `calendar` and refused for the on-pond kinds: a calendar source without them
    /// would look connected and never sync, the empty-source shape `availability` prevents.
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

/// The owner of a source is resolved, never supplied: the body has no `profile_id` field, and the
/// owner comes from `resolve_turn_scope`. Every item the source produces inherits that owner and
/// migration 0044 refuses to let it change. `Household` and `Guest` are refused rather than
/// defaulted -- a guest who could CREATE a source would be writing into a member's corpus.
async fn context_source_owner(
    state: &Arc<AppState>,
    session_id: &str,
    device: &ProvenDevice,
) -> Result<String, (StatusCode, Json<Value>)> {
    let scope = resolve_turn_scope(state, session_id, device).await;
    if let Some(id) = scope.owner_id() {
        return Ok(id.to_string());
    }

    // `Household` in a ONE-MEMBER pond resolves to that member. Deliberately NOT extended to
    // `Guest` or to a household with two or more members, where picking one would attribute an
    // account by row order. Residual exposure: in a one-member pond an unidentified caller on an
    // authenticated-but-unattributed device can connect an account that becomes the member's.
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
    // Refuse a kind whose connector does not exist, HERE, with the domain's own
    // sentence. A source stored now would sit in the table looking connected and
    // never produce anything.
    let availability = kind.availability();
    if availability != pond_core::context::domain::SourceAvailability::Landed {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": availability.refusal(), "kind": kind.as_str()})),
        ));
    }

    // A provider that cannot possibly authenticate is refused HERE, with the
    // reason, rather than stored and left to fail every half hour with a 401
    // that reads like a mistyped password. Google Calendar over CalDAV is the
    // case: its own guide requires OAuth 2.0 and rejects Basic auth.
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
    // Deterministic id, so connecting the same device twice is an update rather than a second
    // source racing the first. An account kind carries the OWNER too: two members each connecting
    // their own Google calendar would collide on `calendar:google`, which migration 0044 refuses.
    // The profile id is already on the row, so this adds no new personal data to the key.
    let id = if kind.needs_credentials() {
        format!("{}:{}:{}", kind.as_str(), body.provider.trim(), owner)
    } else {
        format!("{}:{}", kind.as_str(), body.provider.trim())
    };

    // Credentials, before the source row exists. Storing them second would
    // leave a source that cannot sync if the secret write failed, which is the
    // same empty-source outcome by a slower route.
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
            // No secret store means no credentials, and REFUSING is the only
            // safe answer: the alternatives are dropping the password (a source
            // that can never sync) or putting it somewhere unencrypted, and
            // invariant 4 exists to rule out the second.
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
            // Migration 0044 REFUSES an update that moves a source's owner or kind,
            // with a trigger, because every stored item is denormalised under both.
            // That is a 409 and not a 500: the caller asked for something coherent
            // and the pond is refusing it, which they can act on.
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

/// `GET /api/v1/context/items?session_id=X&q=…` -- what the pond has read.
///
/// Scoped in the SQL rather than filtered afterwards: the caller sees their own items and nobody
/// else's. Keyword search rather than semantic, because a cosine ranking buries an exact match.
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
        // Split on whitespace: the store's keyword search takes terms, and
        // handing it the whole phrase would match only items containing that
        // exact string.
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
                // Whether retrieval can currently reach it. The list is also
                // the place somebody asks "why did search not find this", and
                // the answer is usually this flag.
                "searchable": i.embedding().is_some(),
            }))
            .collect::<Vec<_>>()
    })))
}

/// `POST /api/v1/context/sync` -- pull every connected account now.
///
/// Answers with what the pass actually did, because "checked, nothing new" and "found eleven
/// things" are both successes. Held open for the duration rather than returning a job id.
async fn sync_context_sources(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(syncer) = state.account_sync.as_ref() else {
        // Distinguished from "synced, found nothing": this pond CANNOT sync,
        // and reporting a zero would read as a working account with no news.
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
    // Scoped, not filtered afterwards: `list_sources` takes the scope, and
    // `scope.rs` answers `Guest` with nothing.
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

    // One grouped query for every source, so the counts cost the same whether a
    // household has one account or six. A failure here loses the counts and
    // keeps the list: knowing what is connected matters more than knowing how
    // much each one brought.
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
                // When the pond last reached this account. `null` means it has
                // not run yet, which a household reads very differently from
                // "checked, found nothing" -- the surface needs to tell them
                // apart or a source that has never synced looks healthy.
                "last_sync": s.last_sync().map(|t| t.to_rfc3339()),
                "needs_credentials": s.kind().needs_credentials(),
                // What this source has produced and how much of it retrieval can
                // reach, reported separately: a source can be perfectly connected
                // and still half-invisible while the index catches up, and that gap
                // is what explains a search coming up short.
                "items": stat.map(|st| st.items).unwrap_or(0),
                "awaiting_index": stat.map(|st| st.awaiting_index).unwrap_or(0),
            })
            })
            .collect::<Vec<_>>()
    })))
}

/// `DELETE /api/v1/context/sources/{id}?session_id=X` -- disconnect, and say how many items went.
///
/// PAI-8 invariant 6: disconnecting a source deletes its items by default AND says how many, so
/// the count is in the response body rather than a log line.
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
// `IndexHealth` is computed but was only ever written to a `tracing` line, which is how an index
// populated at roughly 2% survived six phases. Two routes: one to read what retrieval can reach,
// one to force the re-embed that repairs it.

/// Said by both routes below, so they cannot tell different stories about the
/// same pond. "No index" from one and a cleared count from the other would leave
/// a reader unable to say which was true.
const NO_VECTOR_INDEX: &str = "this pond has no vector index, so nothing is embedded and \
                               retrieval falls back to recency";
const NO_EMBEDDING_MODEL: &str = "no embedding model is configured, so nothing has been indexed \
                                  and retrieval falls back to recency";

/// Coverage as a fraction, or `null` when there is nothing to cover.
///
/// `0/0` is neither 0% nor 100% and both readings mislead: zero paints a pond that never stored a
/// memory permanently red, one paints a structurally empty corpus green. `null` says no rows.
fn index_coverage(indexed: u64, rows: u64) -> Option<f64> {
    (rows > 0).then(|| indexed as f64 / rows as f64)
}

/// Every IANA zone, with the offset it is on today.
///
/// Exists so the desktop stops carrying its own divergent lists. Offsets are computed here rather
/// than in the client because an offset depends on the date and a cached one goes wrong at DST.
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

/// Work out where this pond is, from several sources, cheapest first. Server-side so onboarding
/// and Settings share ONE implementation. The network source, the one that would reveal this
/// household's address, is deliberately not wired: everything returned comes from the device's own
/// zone and a geocoding call for a place NAME, which tells the far end nothing about who asked.
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
    // The health query asks "how many rows carry a vector from THIS model", so with no embedder
    // there is no model to ask about. Reporting every row as missing would describe a pond that
    // switched embeddings off exactly as it describes one whose index was wiped, and those two
    // need opposite responses.
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

    // Summed from the rows rather than tracked separately: the three totals are
    // already defined as the per-corpus sums, so deriving the denominator the
    // same way is the only way the overall fraction and the rows can agree.
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
                // Rows exist in the table and NONE of them qualify: not an empty
                // corpus waiting for data but a predicate excluding everything, which
                // no amount of embedding repairs. Measured live: 27 sessions carried a
                // rolling summary, zero qualified, and coverage read 100%.
                "structurally_excluded": c.rows == 0 && c.source_rows > 0,
            }))
            .collect::<Vec<_>>(),
    })))
}

/// `POST /api/v1/context/index/rebuild` -- empty the index so the maintenance sweep fills it
/// again. The sweep is driven by a row's vector being ABSENT, so emptying the table is the only
/// repair after a changed embedder, width or task prefix. Nothing is lost: `0001_vectors.sql`
/// makes every row recomputable. Unlike the health route this does NOT require an embedder.
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

    // Raw SQL against the vectors pool: `VectorIndex` can drop ONE row by id and prune orphans,
    // and neither empties an index whose source rows are all still present, which is every
    // rebuild there will ever be. Rather than widen the port for one caller, the route deletes
    // from the table the port owns, on the pool `AppState` already holds.
    let mut tx = state.db.vectors.begin().await.map_err(db_error)?;

    // Counted BEFORE the delete and inside the same transaction, because the
    // count IS the answer: read afterwards it is always zero, and read outside
    // the transaction a concurrent sweep write can land between the two
    // statements and the report describes a state that never existed.
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

    // Every corpus is listed even at zero: a corpus absent from an answer is a corpus nobody can
    // see is broken. The total comes from the DELETE rather than from summing these, so a row
    // written under a corpus name a later build stopped using is still counted as cleared.
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

    // The sweep that refills is idle-gated and the person who just pressed Reindex is not idle, so
    // clearing without this wake leaves the panel empty; a requested pass skips the quiet gate.
    // `notify_one` rather than `notify_waiters`: there is one sweep, and this variant holds a
    // permit through a mid-pass rebuild so it still gets a fresh pass afterwards.
    let refilling = match state.index_reindex.as_ref() {
        Some(notify) => {
            notify.notify_one();
            true
        }
        // No sweep in this process to wake. Clearing still did something: the
        // next process rebuilds from an empty file, which is the documented way
        // out of a changed embedder.
        None => false,
    };

    Ok(Json(json!({
        "indexed": true,
        "cleared": cleared,
        "refilling": refilling,
        "corpora": corpora,
    })))
}

/// PAI-1 P9's attribution repository, built from the pool `AppState` already holds, the same
/// story as [`proposal_repo`]. It belongs on `AppState` as an injected
/// `Arc<dyn DeviceAttribution>`; everything needing it goes through this one function so that
/// swap is a one-line change here and no change in any handler.
fn device_attribution(
    state: &Arc<AppState>,
) -> pond_infra::sqlite_device_attribution::SqliteDeviceAttribution {
    pond_infra::sqlite_device_attribution::SqliteDeviceAttribution::new(state.db.system.clone())
}

/// The speaking member's own preferences, for the prompt (PAI-1 P6), from the scope
/// [`resolve_turn_scope`] produced: `Owner(id)` uses that member's preferences, `Household` falls
/// back to `settings.primary_profile_id`, and `Guest` gets `None`. A missing profile is `None`,
/// not an error. Name, birthday and language are Owner-scoped; `atypical_speech` is not.
async fn profile_context_for(
    state: &Arc<AppState>,
    scope: &ProfileScope,
) -> Option<ProfileContext> {
    // Whether the turn is attributed to ONE member, which is what makes it safe
    // to state that member's particulars.
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

/// The pure half of [`profile_context_for`]: given a member's stored preferences and whether the
/// turn is attributed to that ONE member, what may the prompt state? Split out so the scoping rule
/// is testable without an `AppState`. The key names are the contract with the writer:
/// `PATCH /api/v1/profiles/{id}` must store them, and a camelCase key changes nothing here.
fn particulars_for(
    attributed: bool,
    prefs: &std::collections::HashMap<String, String>,
) -> ProfileContext {
    ProfileContext {
        // Owner-scoped: facts about one person, stated only when the turn is
        // attributed to that person.
        preferred_name: attributed
            .then(|| prefs.get("preferred_name").cloned())
            .flatten(),
        birthday: attributed.then(|| prefs.get("birthday").cloned()).flatten(),
        language: attributed.then(|| prefs.get("language").cloned()).flatten(),
        // Household-safe: an accommodation, not a disclosure. See the doc on
        // `profile_context_for`.
        atypical_speech: prefs
            .get("accessibility_atypical_speech")
            .map(|v| v == "true")
            .unwrap_or(false),
    }
}

/// Map a session-storage failure from an identity write onto a status code.
///
/// Only a genuinely missing session is a 404. A locked database or a foreign key naming a gone
/// profile is a server fault; reporting it as "no such session" misdirects whoever debugs it.
fn identity_write_error(e: SessionStorageError) -> (StatusCode, Json<Value>) {
    let status = match e {
        SessionStorageError::SessionNotFound(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({"error": e.to_string()})))
}

/// POST /api/v1/sessions/:session_id/identify-user — wake-on-face hook.
///
/// Same multipart payload and body as `/faces/identify`, plus optional `bbox` and `session_id`.
/// A match writes `profile_id` onto the session row; no match or a downgrade leaves it untouched.
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

    // A face match is the WEAKEST source that can bind a session, so it must not silently take
    // over one bound by a paired device or by the member saying so. The remaining race is two
    // concurrent identifications of the same camera frame interleaving within milliseconds, whose
    // outcome is indistinguishable from either winning cleanly.
    let mut bound = false;
    if result.identified {
        if let Some(pid) = result.profile_id.clone() {
            let proposed = SessionIdentity {
                profile_id: Some(pid),
                source: IdentificationSource::Face,
                confidence: result.confidence,
            };
            // One atomic conditional write, not read-compare-write: two requests could both read
            // `Unknown`, both pass `supersedes`, and the later write would win whatever its rank,
            // so a face match landing after somebody tapped "this is Liz" takes the session for a
            // different person on weaker evidence.
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

/// Evaluate, record, and report whether a caller may claim a session belongs to a named member,
/// binding at [`IdentificationSource::Explicit`] strength. The mode is read from settings on every
/// call, never cached. In `audit` mode a refusal still proceeds and `ok` is `true` for exactly the
/// requests `enforce` would block, so the `verdict` field carries `would_deny`.
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

    // A request that reached a protected handler with no principal attached is
    // a wiring fault, not an anonymous caller. Treat it as the least privileged
    // thing available rather than as permission: access narrows on failure
    // (PAI-1 invariant 2).
    let principal = principal.unwrap_or_else(pol::Principal::internal);

    let decision = if pol::is_identity_assertion_proven(&principal, asserted_profile_id) {
        pol::PolicyDecision::permit(mode)
    } else {
        pol::PolicyDecision::refuse(mode, pol::REASON_UNPROVEN_IDENTITY)
    };

    // Tallied at the decision site, not inside an `audit` implementation and not conditionally on
    // an installed policy adapter: this counts what the policy decided. `POLICY_COUNTERS` survives
    // log retention and the "clear my activity" button; the event log survives a restart. Neither
    // is trustworthy alone, so `GET /security/policy-report` reports them separately.
    pol::POLICY_COUNTERS.record(&decision);
    if let Some(policy) = &state.security_policy {
        policy
            .audit(
                &principal,
                // A plain verb. The verdict used to ride this string as a
                // `:{verdict}` suffix; it is an attribute now, so the report
                // groups on a field instead of parsing a substring.
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

    // ── PAI-1 P4 / PAI-2 P1: the policy's first production call site ────────
    // Binds a profile_id from the request BODY at Explicit strength, after which every turn in the
    // session resolves to that member's scope. Nothing can PROVE an identity yet, so `enforce`
    // refuses every remote assertion; the mode ships as `audit`. See is_identity_assertion_proven.
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
        // Explicit identification carries no confidence. It is not "1.0" -- it
        // is a different kind of claim, and a number invites averaging it
        // against a face score.
        confidence: None,
    };

    // Atomic, for the same reason as the face path above.
    let bound = state
        .session_storage
        .set_session_identity_if_stronger(&session_id, &proposed)
        .await
        .map_err(identity_write_error)?;

    if !bound {
        // Report what actually holds the session, read after the refusal so it
        // reflects the state that won rather than a pre-write guess.
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

/// GET /api/v1/sessions/:session_id/user — read the bound profile.
///
/// Reports the evidence alongside the id: a caller needs to know whether "this is Jerry" came
/// from a phone's token or from a 0.6 face match, and a bare profile id cannot say.
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

/// DELETE /api/v1/sessions/:session_id/user — release the binding.
///
/// Always available, whatever bound the session: releasing an attribution narrows what the
/// session may reach, so unlike setting one it needs no strength check.
async fn clear_session_user_handler(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Unlike the identify path, defaulting on a failed read is safe here: this
    // value only decides what the response *reports*, and the release below
    // runs unconditionally. A failure narrows access either way.
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

    /// The address a phone uses to reach this Pond from outside the house.
    ///
    /// The range check carries the whole weight: the routing probe answers even
    /// on a host with no tailnet, so without it the Pond would publish its LAN
    /// address as a remote one and every off-network client would fail.
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
            // 100.63.x and 100.128.x are ordinary public space. Off-by-one here
            // publishes somebody else's address as this Pond's.
            for ip in [
                Ipv4Addr::new(100, 63, 255, 255),
                Ipv4Addr::new(100, 128, 0, 0),
            ] {
                assert!(!is_tailnet_v4(ip), "{ip} is outside 100.64.0.0/10");
            }
        }

        #[test]
        fn rejects_the_lan_addresses_the_probe_returns_without_a_tailnet() {
            // Exactly what the probe answers on a host that merely has a default
            // route, which is the case the check exists to catch.
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

    /// Whisper `.bin` files have no self-describing header, so the filename is
    /// the only source — but it is whisper.cpp's own published convention
    /// rather than a guess, and it must not label unrelated `.bin` files.
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

        /// The guard that matters: a stray `.bin` in the models folder is not a
        /// whisper model, and labelling it one puts it under Listening with a
        /// size it does not have.
        #[test]
        fn claims_nothing_about_a_file_it_does_not_recognise() {
            assert_eq!(whisper_facts_from_name("some-random-weights"), (None, None));
            assert_eq!(whisper_facts_from_name(""), (None, None));
        }
    }
    use super::*;

    /// Shaping a Spotify playback body for the dashboard widget — the part of
    /// `music_now_playing_handler` that stays pure and can be pinned with
    /// hand-built fixtures instead of a mocked `api.spotify.com`.
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

        /// A podcast episode has no `artists` array — reading it the
        /// track-shaped way silently landed on "" for both fields even when
        /// Spotify DID send episode data. Show name and its own top-level
        /// `images` are the fix, not the `album.images` a track uses.
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

        /// Spotify reports `currently_playing_type: "episode"` and `is_playing: true` with
        /// `item: null` -- a real gap in Spotify's own API, not a parse failure. Track/artist
        /// must read as an honest explanation, never a blank field read as a broken widget.
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

        /// Paused (not playing) with no item must NOT get an invented label —
        /// that fallback exists to explain an active, otherwise-silent
        /// player, not to narrate an idle one.
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

    /// The tools this path must never expose. Each one decides on the caller's behalf, executes,
    /// or reads household data, and the direct-dispatch routes carry no caller identity to check
    /// it against. `giap-orchestrator__delegate` is now the sharpest; the two `giap-draft` entries
    /// that used to head this list went with their group, and an entry naming a tool that no
    /// longer exists asserts nothing.
    const MUST_NEVER_BE_DIRECTLY_DISPATCHABLE: &[&str] = &[
        "giap-memory__recall_memories",
        "giap-memory__save_memory",
        "giap-schedule__create_schedule",
        // PAI-6 P5. Direct dispatch runs a tool with no chat turn, so no engine session in
        // `_meta` and no `DelegationAuthority` to resolve. An allowlist entry here is reachable
        // by any paired client and any sandboxed MCP App iframe, and a delegation is a multi-turn
        // autonomous run on household hardware decided by an authority this path does not have.
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
        // `qualify_tool_name` only prepends a server when one was supplied, so a
        // bare `tool` with an empty `server` reaches the gate unqualified. If an
        // entry here were bare, that request would match it.
        for tool in DIRECT_DISPATCH_ALLOWLIST {
            assert!(
                tool.contains("__"),
                "{tool} is not server-qualified, so an unqualified request matches it"
            );
        }
    }

    #[test]
    fn the_hub_can_still_actuate_a_device() {
        // hubStore.ts and Rooms.tsx both post {server, tool} rather than a
        // qualified name — the gate sees whatever `qualify_tool_name` produced.
        let qualified = qualify_tool_name("giap-device-control", "set_device_state");
        assert!(DIRECT_DISPATCH_ALLOWLIST.contains(&qualified.as_str()));
    }

    // ── sensor rules (PAI-7 P8) ──────────────────────────────────

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
            // The whole point of the id resolution: `/rules/{id}` must not be a second door onto
            // `/schedules`. This is the PROJECTION only: dropping `&& rule_view(t).is_some()` from
            // `find_rule` leaves this crate's lib suite green while PUT at a cron schedule's id
            // answers 200. Consumers: `tests/rules_surface_test.rs`, through the router.
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
            // Vacuity control: the same projection over a real rule works, so
            // "None" above is about the KIND and not about the fixture.
            assert!(rule_view(&schedule("r", TaskKind::SensorTrigger(spec()))).is_some());
        }

        #[test]
        fn the_rule_view_reports_the_durable_cooldown_stamp() {
            // "Why has my rule not fired?" is answered by the persisted fire
            // stamp (PAI-7 P8's first repair) and by nothing else on this
            // response. A view that dropped it would send the user to the logs.
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
            // The shape `/schedules` forced was a schedule wrapping a kind
            // wrapping a spec, plus a cron expression the scheduler never
            // reads for an event rule.
            let body = json!({
                "name": "Backyard motion after sunset",
                "source": {"kind": "sensor", "device_id": "backyard-pir", "signal": "motion"},
                "condition": {"after": "18:30", "before": "06:00"},
                "actions": [{"type": "notify", "title": "Motion", "body": "Backyard"}]
            });
            let req: ApiRuleRequest = serde_json::from_value(body).expect("flattened spec parses");
            assert_eq!(req.name, "Backyard motion after sunset");
            assert!(req.id.is_none());
            // Not supplied, so it takes the domain default rather than 0 —
            // which would be no debounce at all on a flapping PIR.
            assert_eq!(req.spec.cooldown_secs, 60);
            assert_eq!(req.spec.condition.after.as_deref(), Some("18:30"));
            assert!(req.spec.validate().is_ok());
        }

        /// This file, for the ORDER guard below. The guard is a TRIPWIRE, not the coverage:
        /// `tests/rules_surface_test.rs` asserts through the router what refuses a rule that can
        /// never fire. This asks only that each handler calls the check, BEFORE the store, and
        /// RETURNS its answer -- a discarded result would still pass an offset comparison.
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
            // Vacuity control for the guard below: if `handler_body` returned
            // the whole file, every ordering assertion would pass by accident,
            // satisfied by some other handler's code.
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

        /// The span between one handler's rejection call and its store call —
        /// the window the RETURN assertion below searches, and the one its
        /// vacuity control measures. One function, so the control cannot be
        /// measuring a window the guard does not use.
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
                // The pre-existing door. A rule can still be created here by
                // naming the kind, so skipping it would leave the new
                // surface's validation trivially avoidable.
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
                // And the answer is RETURNED. `if let Some((_status, _body)) =
                // rule_spec_rejection(..) { debug!(..) }` is a call, is before
                // the store, and refuses nothing.
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
            // Vacuity control for the RETURN assertion: it searches the span BETWEEN the check
            // and the store. Every handler returns a `(status, Json(body))` tuple further down,
            // so a window grown to the whole body would be satisfied by that and pass against a
            // rejection whose answer is discarded.
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
            // The handlers call `validate`; this pins the cases they must be
            // refusing, so the two cannot drift into "accepted and silent".
            let mut bad = spec();
            bad.actions.clear();
            assert!(bad.validate().is_err());

            let mut bad = spec();
            bad.condition.after = Some("half six".into());
            assert!(bad.validate().is_err());
        }
    }

    // ── image attachment limits (phase F1) ───────────────────────

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

    /// The message must be actionable, not a bare status. It carries the real
    /// numbers so the UI can say what to do about it.
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
        // A development-mode app serves only allowlisted accounts, and the
        // whole OAuth flow succeeds for everyone else — so the message has to
        // point at the developer dashboard, not at the connection.
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
        // Settings page sends the whole object: the new name plus the OLD
        // coordinates (echoed, == current). Those coordinates are stale for the
        // new city, so we must still geocode.
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
        // Patched coordinates differ from stored → an explicit edit; respect it
        // even though the name also implies a different place.
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

    /// A real `giap-knowledge__compute_answer` result, captured verbatim from `pond-mcp-server`'s
    /// `print_a_real_rendered_result`. The marker ends at the FIRST `]]]` and splits on the FIRST
    /// `:`, and this payload carries a `https://` URL full of colons and nested objects; a result
    /// whose own text contained `]]]` would cut the marker short, so the producer substitutes it.
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
        // This string is what the frontend looks the card up by
        // (`findCardByHint`), so it is the whole contract with WolframCard.tsx.
        assert_eq!(ui["card_type"], "wolfram");
        assert_eq!(ui["data"]["primary"], "4.828 km");
        // The URL's own colons must not have been mistaken for the separator.
        assert_eq!(
            ui["data"]["source_url"],
            "https://www.wolframalpha.com/input?i=3%20miles%20in%20km"
        );
        // The suggestion ids are what `explore_computation` resolves; losing
        // them here would leave the card's chips pointing at nothing.
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
        // Marker present but payload has no colon separating type from JSON
        let input = "[[[mcp-ui:weather]]]\nSome text";
        let (clean, hint) = extract_ui_hint(input);
        assert_eq!(clean, "Some text");
        assert!(hint.is_none(), "missing colon should not produce a hint");
    }

    // ── The stream translator ────────────────────────────────────────────
    // PAI-5 P7. `TurnAccumulator::absorb` is the only place an `AgentStreamEvent` becomes an SSE
    // frame, for BOTH routes; `stream_handler_parity.rs` covers that no second match appears.
    // Compare frames WHOLE: a `tool`/`id` swap in `tool_result` keeps the type, matching no card.

    use pond_core::models::ports::agent::AgentStreamEvent;
    use pond_core::models::ports::provider::UsageStats;
    // Not re-exported through `models::ports::agent`, which names the three
    // types the port's signatures need and nothing else.
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

    /// Long enough to clear the filter's lookahead, which holds back a tail
    /// that might turn out to be the start of a `<|channel>` tag. A short
    /// fixture emits nothing and would make every assertion here vacuous.
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
        // The desktop reads `token`; the hub reads `content`. Both, or one of
        // the two surfaces renders an empty message.
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

    /// PAI-3 and PAI-4 read this timing off `TurnMetrics`. It is gathered
    /// nowhere else, so deleting it here is a silent regression in a workstream
    /// that has no test in this crate.
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
                // Carried from the ToolCall event, which is the only place they
                // appear -- the ToolResult event does not repeat them. Without
                // this the stored tool-call record names a call nobody can
                // reproduce.
                "arguments": "{\"location\":\"Nairobi\"}",
            }),
            "the persisted row is replayed into the next prompt as the model's own \
             tool history; a row whose id and name are transposed teaches the model \
             it called a tool named `call-1`"
        );
    }

    /// The MCP-UI marker is a rendering instruction, not something to keep.
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

    /// The reasoning comes back so the HANDLER can offer it to its own `ChatService`, which holds
    /// the `persist_thinking` gate. The translator is handed no service on purpose: recording here
    /// would let both routes inherit a decision neither can see at its call site, reaching the
    /// PAI-5 P6 gate from a function that does not know whose turn it is.
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

    /// `Done` is the one variant the two routes disagree about, so it must not arrive as a frame:
    /// `/chat/stream` would send a `done` before persisting anything, ahead of its real `done`
    /// carrying the usage totals, while `/agent/chat/stream` would send two.
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

    /// Every remaining variant is a frame carrying the `type` the desktop switches on and the
    /// payload that type promises. The `type` alone is not the contract: `review_revision` carries
    /// two adjacent integers meaning opposite things (score, rounds), so the fixture uses 4 and 2
    /// rather than one number twice, and transposing them fails here.
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

        // The error frame is the exception: it carries no `type` at all, and
        // the client detects it by the presence of `error`. Changing that to a
        // typed frame would silently stop every existing client from showing
        // failures.
        let err = frame_of(turn.absorb(AgentStreamEvent::Error {
            content: "the model went away".to_string(),
        }));
        assert_eq!(err["error"], "the model went away");
        assert!(err.get("type").is_none());
    }

    /// PAI-6 P6 / invariant 4: a subagent's activity reaches the CLIENT and never the parent's
    /// history. `full_text` and `tool_results` are what both handlers persist, so a progress frame
    /// touching either puts a child's conversation into `session_messages`. Tool timing is
    /// asserted too: reusing the `ToolCall` arm reports the CHILD's tool in `TurnMetrics`.
    #[test]
    fn absorb_progress_leaves_the_turn_untouched() {
        let mut turn = accumulator();
        // A prior real tool call, so the assertions below distinguish "the
        // progress frame did not write" from "nothing was ever written".
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

    /// The keys the reader expects, spelled exactly as the writer must store
    /// them. Written out rather than referenced so a rename on either side
    /// fails a test instead of silently reading nothing.
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

    /// An attributed turn may state the member's own particulars.
    #[test]
    fn an_owner_scoped_turn_states_the_members_particulars() {
        let ctx = particulars_for(true, &full_prefs());
        assert_eq!(ctx.preferred_name.as_deref(), Some("Cap"));
        assert_eq!(ctx.birthday.as_deref(), Some("1990-04-02"));
        assert_eq!(ctx.language.as_deref(), Some("sw"));
        assert!(ctx.atypical_speech);
    }

    /// An UNATTRIBUTED turn must not. `ProfileScope::Household` falls back to
    /// `primary_profile_id`, so without this the pond announces the primary
    /// member's name and birthday while somebody else is talking.
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

    /// The one field that deliberately crosses into an unattributed turn: the
    /// speech accommodation discloses nothing about anybody, and a household
    /// that configured it wants it applied precisely when the pond cannot tell
    /// who is speaking. Asserted separately so narrowing it is a visible choice.
    #[test]
    fn the_speech_accommodation_survives_an_unattributed_turn() {
        assert!(
            particulars_for(false, &full_prefs()).atypical_speech,
            "the speech accommodation was dropped for unattributed turns; it is not a \
             disclosure and dropping it makes the pond less patient with the household it was \
             configured for"
        );
    }

    /// Absent keys are absent, not empty strings -- the prompt builder skips
    /// `None` and would render "The user's birthday is ." for a `Some("")`.
    #[test]
    fn a_profile_with_no_preferences_yields_nothing_to_state() {
        let ctx = particulars_for(true, &std::collections::HashMap::new());
        assert_eq!(ctx.preferred_name, None);
        assert_eq!(ctx.birthday, None);
        assert_eq!(ctx.language, None);
        assert!(!ctx.atypical_speech);
    }

    /// The camelCase trap, stated as a test. The desktop app holds these as
    /// `preferredName` / `atypicalSpeech`; if a writer stores those spellings,
    /// `PATCH /profiles/{id}` returns 200 and the row looks populated while the
    /// prompt still says nothing.
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

    /// Every group `recipe_extension_to_tool_group` can name must be a group
    /// that exists.
    ///
    /// A mapping to a non-existent group is worse than no mapping at all. The
    /// name is RECOGNISED, so it is not dropped with the warning the function's
    /// doc promises; it goes into `tool_group_allowlist`, and the filter in
    /// `goose_agent`'s recipe branch then keeps the tools whose prefix matches
    /// it -- of which there are none. The recipe runs with zero tools and the
    /// only trace is a `recipe_tools_restricted` line saying `kept = 0`.
    ///
    /// Two mappings were in that state: `giap-vision`, whose group was deleted,
    /// and `giap-matter`, which was never a tool group at all -- it is the
    /// matter.js websocket protocol name.
    #[test]
    fn every_recipe_extension_maps_to_a_tool_group_that_exists() {
        // The goose-side names this function is willing to translate. Kept
        // literal rather than derived: the point is to exercise the match arms.
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

        // Vacuity control: if the arms were renamed, every lookup would return
        // None and the loop above would panic -- but if the list were emptied,
        // it would pass having asserted nothing.
        assert_eq!(
            mapped,
            RECIPE_EXTENSION_NAMES.len(),
            "the mapping list is not exercising the match arms"
        );
    }
}
