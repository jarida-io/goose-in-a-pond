//! Route definitions for GIAP REST API and web dashboard.
//!
//! # TODO
//! - [ ] Implement each handler with real logic
//! - [ ] Add request/response types in pond-core domain
//! - [ ] Serve static web dashboard files

use axum::{
    body::Body,
    extract::{rejection::JsonRejection, Multipart, Path, State},
    http::{Response, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Json,
    },
    routing::{delete, get, patch, post, put},
    Router,
};
use pond_core::domain::message::ChatMessage;
use pond_core::domain::profile::CreateProfileRequest;
use pond_core::domain::sensor::{CameraEvent, SensorReading};
use pond_core::ports::scheduler::CreateTaskRequest;
use pond_core::domain::settings::Settings;
use pond_core::ports::extension_manager::ExtensionInfo;
use pond_core::ports::device_registry::RegisterDeviceRequest;
use tower_http::services::ServeDir;
use pond_core::domain::onboarding::OnboardingStep;
use pond_core::ports::handshake::{HandshakeRequest, HandshakeResponse};
use pond_core::prompts::{build_system_prompt_with_profile, render_template, sanitize_field, ProfileContext};
use pond_core::ports::provider::LlmProvider;
use pond_core::services::chat::ChatService;
use pond_core::services::model_router::ModelRouter;
use pond_core::services::onboarding::OnboardingService;
use pond_core::services::request_classifier::classify_request;
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::Arc;
use uuid::Uuid;

use pond_core::domain::model_record::{ModelCategory, ModelRecord, ModelRoleAssignment};
use pond_core::domain::memory::MemoryFragment;
use pond_core::domain::prompt_extra::PromptExtra;
use pond_core::domain::prompt_template::PromptTemplate;
use pond_core::domain::recipe::AgentRecipe;
use pond_core::domain::skill::UserSkill;

use crate::{AppState, DownloadEntry, ModelStatusEntry};
use crate::middleware::onboarding_guard::require_onboarding_complete;
use pond_adapters_whisper;

// ───────────────────────── REST API Routes ─────────────────────────

/// Builds the full REST API router with onboarding-aware middleware
pub fn api_routes(state: Arc<AppState>) -> Router<Arc<AppState>> {
    // ───────────── Public routes (accessible before onboarding) ─────────────
    let public_routes = Router::new()
        .route("/health", get(health))
        .route("/handshake", post(handshake_handler))
        .route("/onboard", post(start_onboarding))
        .route("/onboard/complete", post(complete_onboarding))
        .route("/onboard/status", get(onboarding_status))
        // Settings write is public so onboarding steps can save before completion
        .route("/settings", put(update_settings))
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
        .route("/tts", post(tts_synthesise))
        .route("/sessions", get(list_sessions))
        .route("/sessions/{session_id}", patch(rename_session))
        .route("/sessions/{session_id}/messages", get(get_session_messages))
        .route("/devices", get(list_devices).post(register_device))
        .route("/devices/{id}", axum::routing::delete(unregister_device))
        .route("/devices/{id}/heartbeat", post(device_heartbeat))
        .route("/settings", get(get_settings))
        .route("/models", get(list_models))
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
        .route("/models/{category}/{name}/download", post(download_model))
        .route("/models/{category}/{name}/activate", post(activate_model))
        .route("/models/{category}/{name}", delete(delete_model))
        .route("/profiles", get(list_profiles))
        .route("/profiles/{id}", get(get_profile).delete(delete_profile))
        .route("/profiles/{id}/enroll", post(enroll_speaker))
        .route("/profiles/{id}/biometrics", delete(delete_speaker_biometrics))
        .route("/sensors", post(record_sensor))
        .route("/sensors/{device_id}", get(get_recent_sensors))
        .route("/camera/events", get(list_camera_events).post(record_camera_event))
        .route("/camera/events/{id}/acknowledge", patch(acknowledge_camera_event))
        // ── Scheduler ──────────────────────────────────────────────────────────
        .route("/schedules", get(list_schedules).post(create_schedule))
        .route("/schedules/{id}", delete(delete_schedule))
        .route("/schedules/{id}/pause", post(pause_schedule))
        .route("/schedules/{id}/resume", post(resume_schedule))
        .route("/schedules/{id}/run-now", post(run_schedule_now))
        // ── Extensions (MCP/Goose extension manager) ───────────────────────────
        .route("/extensions", get(list_extensions_handler).post(add_extension_handler))
        .route("/extensions/{name}", delete(remove_extension_handler).patch(toggle_extension_handler))
        // ── Prompt Templates ───────────────────────────────────────────────────
        .route("/prompts", get(list_prompt_templates))
        .route("/prompts/{name}", get(get_prompt_template).put(upsert_prompt_template).delete(delete_prompt_template))
        // ── Agent Tools (MCP) ─────────────────────────────────────────────────
        .route("/agent/tools", get(list_agent_tools))
        // ── Agent chat stream (agentic tool-use loop) ─────────────────────────
        .route("/agent/chat/stream", post(agent_chat_stream))
        // ── System Prompt Extras ───────────────────────────────────────────────
        .route("/agent/extras", get(list_prompt_extras).post(upsert_prompt_extra))
        .route("/agent/extras/{key}", delete(delete_prompt_extra))
        // ── Event log / telemetry ─────────────────────────────────────────────
        .route("/logs", get(list_logs))
        .route("/logs/export", get(export_logs_csv))
        // ── Memories ──────────────────────────────────────────────────────────
        .route("/memories", get(list_memories).post(save_memory))
        .route("/memories/{id}", delete(delete_memory))
        // ── Skills ────────────────────────────────────────────────────────────
        .route("/skills", get(list_skills).post(create_skill))
        .route("/skills/{id}", put(update_skill).delete(delete_skill))
        // ── Recipes ───────────────────────────────────────────────────────────
        .route("/recipes", get(list_recipes).post(create_recipe))
        .route("/recipes/{id}", put(update_recipe).delete(delete_recipe))
        .layer(
            axum::middleware::from_fn_with_state(state.clone(), require_onboarding_complete)
        );

    // Merge public and protected routes, attach shared state
    public_routes
        .merge(protected_routes)
        .with_state(state)
}

// ───────────────────────── Web Dashboard Routes ─────────────────────

/// Serves the built Vite assets from the given directory as a fallback service.
/// In development, use `npm run dev` instead (Vite dev server on port 5173).
pub fn web_routes(static_dir: std::path::PathBuf) -> ServeDir {
    ServeDir::new(static_dir)
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

    let response = state
        .handshake
        .handshake(request)
        .await
        .map_err(|e| {
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

/// Return current onboarding progress (public)
async fn onboarding_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let service = OnboardingService::new(state.onboarding_repo.clone());

    let total_steps = 9;  // Welcome Basics Location Accessibility Personality GooseIdentity WakeWord Model Extensions
    let (current_step, steps_completed, onboarded) = match service.status().await {
        None                                   => ("not_started".to_string(),                    0, false),
        Some(OnboardingStep::Welcome)           => (OnboardingStep::Welcome.to_string(),          1, false),
        Some(OnboardingStep::Basics)            => (OnboardingStep::Basics.to_string(),           2, false),
        Some(OnboardingStep::Location)          => (OnboardingStep::Location.to_string(),         3, false),
        Some(OnboardingStep::Accessibility)     => (OnboardingStep::Accessibility.to_string(),    4, false),
        Some(OnboardingStep::Personality)       => (OnboardingStep::Personality.to_string(),      5, false),
        Some(OnboardingStep::GooseIdentity)     => (OnboardingStep::GooseIdentity.to_string(),    6, false),
        Some(OnboardingStep::WakeWord)          => (OnboardingStep::WakeWord.to_string(),         7, false),
        Some(OnboardingStep::Model)             => (OnboardingStep::Model.to_string(),            8, false),
        Some(OnboardingStep::Extensions)        => (OnboardingStep::Extensions.to_string(),       8, false),
        Some(OnboardingStep::Completed)         => ("Completed".to_string(),                      8, true),
    };

    Json(json!({
        "onboarded": onboarded,
        "current_step": current_step,
        "steps_completed": steps_completed,
        "total_steps": total_steps
    }))
}

#[derive(Deserialize)]
struct ChatRequest {
    session_id: Option<String>,
    message: String,
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

    // Classify the message to determine which model role will handle it
    let model_role = {
        use pond_core::domain::model_role::ModelRole;
        match classify_request(&req.message) {
            ModelRole::Think => "think",
            ModelRole::Task  => "task",
            ModelRole::Chat  => "chat",
        }
    };

    // Build ChatService — agent is always primary (GooseAdapter builds system
    // prompt from DB settings, manages history, handles MCP tools internally).
    let service = ChatService::new(
        state.agent.clone(),
        session_id.clone(),
        storage.clone(),
    );

    let response_text = service
        .chat_once(req.message)
        .await
        .map_err(|e| {
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
        "- If a tool fails, explain what failed and continue with the best possible answer.".to_string(),
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
            lines.push(format!("- {} ({}) - {}", ext.name, ext.kind, ext.description.trim()));
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
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)> {
    use futures::StreamExt;
    use pond_core::ports::agent::AgentStreamEvent;
    let Json(req) = body.map_err(|e| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()})))
    })?;

    let stream = async_stream::stream! {
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

            let file_template = state
                .prompt_template_dir
                .as_ref()
                .and_then(|dir| std::fs::read_to_string(dir.join("system.md")).ok());

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

        // Classify message for model role
        let model_role = {
            use pond_core::domain::model_role::ModelRole;
            match classify_request(&req.message) {
                ModelRole::Think => "think",
                ModelRole::Task  => "task",
                ModelRole::Chat  => "chat",
            }
        };

        // Persist user message
        {
            use pond_core::domain::message::ChatMessage;
            use pond_core::domain::session::SessionMessage;
            let user_msg = ChatMessage::user(req.message.clone());
            let sm = SessionMessage::new(Uuid::new_v4().to_string(), session_id.clone(), user_msg);
            if let Err(e) = storage.add_message(session_id.clone(), sm).await {
                let data = json!({"error": format!("Failed to persist user message: {}", e)}).to_string();
                yield Ok(Event::default().data(data));
                return;
            }
        }

        // ── On-demand llamafile startup ─────────────────────────────────────
        // If any role uses llamafile and the process is not responding, emit a
        // status event and wait up to 90 s before attempting to stream.
        {
            let is_llamafile_role = settings.chat_provider == "llamafile"
                || settings.think_provider.as_deref() == Some("llamafile")
                || settings.task_provider.as_deref()  == Some("llamafile");

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

        let usage_prompt_tokens: u32 = 0;
        let usage_completion_tokens: u32 = 0;

        let model_name_for_done = match model_role {
            "think" => settings
                .think_model
                .as_deref()
                .unwrap_or(settings.chat_model.as_str())
                .to_string(),
            "task" => settings
                .task_model
                .as_deref()
                .unwrap_or(settings.chat_model.as_str())
                .to_string(),
            _ => settings.chat_model.clone(),
        };

        use pond_core::domain::agent::AgentRequest;
        let agent_req = AgentRequest {
            message: req.message.clone(),
            session_id: session_id.clone(),
            model_role: model_role.to_string(),
        };

        let mut full_text = String::new();
        let mut in_think_block = false; // filter <think> blocks before SSE
        let mut agent_stream = match state.agent.chat_stream(agent_req).await {
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
                    let data = match event {
                        AgentStreamEvent::Status { content } => {
                            json!({"type": "status", "content": content}).to_string()
                        }
                        AgentStreamEvent::ToolCall { tool, id, input } => {
                            json!({"type": "tool_call", "tool": tool, "id": id, "input": input}).to_string()
                        }
                        AgentStreamEvent::ToolResult { tool, id, content } => {
                            json!({"type": "tool_result", "tool": tool, "id": id, "content": content}).to_string()
                        }
                        AgentStreamEvent::Text { content } => {
                            // Filter <think>…</think> blocks before sending to clients.
                            let (visible, new_state) = pond_core::services::chat::filter_thinking(&content, in_think_block);
                            in_think_block = new_state;
                            if visible.is_empty() {
                                continue;
                            }
                            full_text.push_str(&visible);
                            json!({"type": "text", "content": visible}).to_string()
                        }
                        AgentStreamEvent::Done { .. } => {
                            // Handled at the end of the loop
                            continue;
                        }
                        AgentStreamEvent::Error { content } => {
                            json!({"error": content}).to_string()
                        }
                    };
                    yield Ok(Event::default().data(data));
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    let data = json!({"error": err_msg}).to_string();
                    yield Ok(Event::default().data(data));
                    return;
                }
            }
        }

        // Persist full assistant response
        {
            use pond_core::domain::message::ChatMessage;
            use pond_core::domain::session::SessionMessage;
            let assistant_msg = ChatMessage::assistant(full_text);
            let sm = SessionMessage::new(Uuid::new_v4().to_string(), session_id.clone(), assistant_msg);
            let _ = storage.add_message(session_id.clone(), sm).await;
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

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// List all sessions, ordered by most recently updated first.
async fn list_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let sessions = state
        .session_storage
        .list_sessions()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to list sessions: {}", e)})),
            )
        })?;

    let session_list: Vec<Value> = sessions
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "title": s.title,
                "created_at": s.created_at.to_rfc3339(),
                "updated_at": s.updated_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(json!({ "sessions": session_list })))
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
                pond_core::ports::session_storage::SessionStorageError::SessionNotFound(_) => {
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

/// Get all messages for a session.
///
/// GET /api/v1/sessions/:session_id/messages
async fn get_session_messages(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    use pond_core::domain::message::Role;
    use pond_core::ports::session_storage::SessionStorageError;

    let messages = state
        .session_storage
        .get_messages(&session_id)
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
            };
            json!({
                "id": m.id,
                "session_id": m.session_id,
                "role": role,
                "content": m.message.content,
                "created_at": m.created_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(json!({ "messages": list })))
}

async fn system_info() -> Json<Value> {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    Json(json!({
        "hostname": hostname,
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    }))
}

async fn list_devices(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let devices = state.device_registry.list_devices().await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    let list: Vec<Value> = devices
        .iter()
        .map(|d| json!({
            "id":            d.id,
            "name":          d.name,
            "device_type":   d.device_type,
            "hostname":      d.hostname,
            "ip_address":    d.ip_address,
            "capabilities":  d.capabilities,
            "registered_at": d.registered_at,
            "last_seen":     d.last_seen,
            "is_online":     d.is_online,
        }))
        .collect();
    Ok(Json(json!({ "devices": list })))
}

async fn register_device(
    State(state): State<Arc<AppState>>,
    body: Result<Json<RegisterDeviceRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Invalid request: {}", e)})))
    })?;
    let device = state.device_registry.register(req).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok((StatusCode::CREATED, Json(json!({
        "id":            device.id,
        "name":          device.name,
        "device_type":   device.device_type,
        "capabilities":  device.capabilities,
        "registered_at": device.registered_at,
        "is_online":     device.is_online,
    }))))
}

async fn unregister_device(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    state.device_registry.unregister(&id).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn device_heartbeat(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state.device_registry.heartbeat(&id).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok(Json(json!({ "status": "ok" })))
}

async fn get_settings(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let settings = state
        .settings_repo
        .get()
        .await
        .map_err(|e| {
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
    let current = state
        .settings_repo
        .get()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to load current settings: {}", e)})),
            )
        })?;

    // Merge: serialise current → Value, apply patch fields, deserialise back.
    let mut base = serde_json::to_value(&current).unwrap_or(serde_json::Value::Object(Default::default()));
    if let (Some(base_obj), Some(patch_obj)) = (base.as_object_mut(), patch.as_object()) {
        for (k, v) in patch_obj {
            base_obj.insert(k.clone(), v.clone());
        }
    }
    let merged: Settings = serde_json::from_value(base).unwrap_or(current);

    state
        .settings_repo
        .update(&merged)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to save settings: {}", e)})),
            )
        })?;

    // Hot-reload the ModelRouter whenever any provider/model field changes.
    let provider_keys = ["chat_provider","chat_model","think_provider","think_model",
                         "task_provider","task_model",
                         "active_whisper_model","active_tts_model"];
    if let Some(obj) = patch.as_object() {
        if obj.keys().any(|k| provider_keys.contains(&k.as_str())) {
            rebuild_model_router(&state, &merged).await;

            // Sync role fields → model_role_assignments (source of truth).
            // This ensures CLI `models list` and `/activate` see the same state
            // as the Settings page write path.
            if let Some(repo) = &state.model_repo {
                let role_map: &[(&str, &str, &str)] = &[
                    ("chat",  &merged.chat_provider,  &merged.chat_model),
                    ("think", merged.think_provider.as_deref().unwrap_or(""), merged.think_model.as_deref().unwrap_or("")),
                    ("task",  merged.task_provider.as_deref().unwrap_or(""),  merged.task_model.as_deref().unwrap_or("")),
                    ("asr",  "", &merged.active_whisper_model),
                    ("tts",  "", &merged.active_tts_model),
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
    Ok(Json(serde_json::to_value(&merged).unwrap_or(json!({ "status": "ok" }))))
}

/// Rebuild and hot-swap the ModelRouter using the new settings.
/// Called whenever the user changes any provider/model assignment.
async fn rebuild_model_router(state: &Arc<AppState>, settings: &Settings) {
    use pond_adapters_llamafile::LlamafileProvider;
    use pond_adapters_ollama::OllamaProvider;
    #[allow(unused_imports)]
    use pond_core::ports::provider::LlmProvider as _;

    let url = &state.llamafile_url;
    let data_dir = state.data_dir.clone();
    let max_tokens   = settings.llm_max_tokens;
    let temperature  = settings.llm_temperature;

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
                    None      => LocalInferenceLlmAdapter::new(model).await,
                };
                match result {
                    Ok(adapter) => Arc::new(adapter) as Arc<dyn LlmProvider>,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to build LocalInferenceLlmAdapter for '{}': {}; \
                             falling back to llamafile",
                            model, e
                        );
                        Arc::new(LlamafileProvider::new(Some(url))
                            .with_max_tokens(max_tokens)
                            .with_temperature(temperature)) as Arc<dyn LlmProvider>
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
    let effective_chat_model    = settings.chat_model.clone();

    let chat = build_one(&effective_chat_provider, &effective_chat_model,
                         url, data_dir.clone(), max_tokens, temperature).await;
    let think = if let (Some(tp), Some(tm)) = (&settings.think_provider, &settings.think_model) {
        build_one(tp, tm, url, data_dir.clone(), max_tokens, temperature).await
    } else { chat.clone() };
    let task  = if let (Some(tp), Some(tm)) = (&settings.task_provider, &settings.task_model) {
        build_one(tp, tm, url, data_dir, max_tokens, temperature).await
    } else { chat.clone() };

    // If any role uses llamafile, ensure the process is running before
    // the new router goes live (so the first request doesn't time out).
    let any_llamafile = effective_chat_provider == "llamafile"
        || settings.think_provider.as_deref() == Some("llamafile")
        || settings.task_provider.as_deref()  == Some("llamafile");

    if any_llamafile {
        if let Some(manager) = &state.llamafile_manager {
            tracing::info!("llamafile provider selected — ensuring server is running");
            let model_hint = if effective_chat_provider == "llamafile" {
                Some(effective_chat_model.as_str())
            } else {
                None
            };
            manager.ensure_started(model_hint).await;
        } else {
            tracing::warn!(
                "llamafile provider selected but no LlamafileManager wired in AppState; \
                 process will not be auto-started"
            );
        }
    }

    let new_router: Arc<dyn LlmProvider> = Arc::new(ModelRouter::new(chat, think, task));
    *state.llm_provider.write().await = Some(new_router);
    tracing::info!("ModelRouter hot-reloaded: chat={}/{} think={:?}/{:?} task={:?}/{:?}",
        effective_chat_provider, effective_chat_model,
        settings.think_provider, settings.think_model,
        settings.task_provider, settings.task_model,
    );
}

// ── Model registry handlers ───────────────────────────────────────────────────

/// GET /api/v1/models/active-roles — returns the provider+model currently wired for each role.
///
/// Reads from `model_role_assignments` (source of truth) with a settings KV fallback.
async fn get_active_roles(
    State(state): State<Arc<AppState>>,
) -> Json<Value> {
    // Try to read from the persistent join table first
    let assignments: std::collections::HashMap<String, String> = state.model_repo
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
    let chat_model    = settings.chat_model.clone();

    Json(json!({
        "chat":  {
            "provider": chat_provider,
            "model":    chat_model,
            "model_id": assignments.get("chat"),
        },
        "think": {
            "provider": settings.think_provider,
            "model":    settings.think_model,
            "model_id": assignments.get("think"),
        },
        "task":  {
            "provider": settings.task_provider,
            "model":    settings.task_model,
            "model_id": assignments.get("task"),
        },
        "asr": { "model_id": assignments.get("asr") },
        "tts": { "model_id": assignments.get("tts") },
        "router_name": state.llm_provider.read().await
            .as_ref()
            .map(|p| p.model_name())
            .unwrap_or_else(|| "none".to_string()),
    }))
}

/// GET /api/v1/models/memory-status — returns current LLM memory budget snapshot.
async fn get_memory_status(
    State(state): State<Arc<AppState>>,
) -> Json<Value> {
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
        category:         m.category.as_str().to_string(),
        name:             m.name.clone(),
        description:      m.description.clone(),
        size_mb:          m.size_mb,
        downloaded:       m.downloaded,
        active,
        url:              m.url.clone(),
        hf_id:            m.hf_id.clone(),
        filename:         m.filename.clone(),
        ram_estimate_mb:  m.ram_estimate_mb,
        recommended_role: m.recommended_role.clone(),
    }
}

/// Scans model directories for files on disk not yet in the catalog,
/// inserts them as custom entries via the model repository, and returns
/// the newly discovered records.
async fn scan_filesystem_extras(
    data_dir: &std::path::Path,
    model_repo: &Arc<dyn pond_core::ports::model_repository::ModelRepository + Send + Sync>,
) -> Vec<ModelRecord> {
    let all = model_repo.list_all().await.unwrap_or_default();
    let known_filenames: std::collections::HashSet<String> = all.iter()
        .filter_map(|m| m.filename.clone())
        .collect();

    let scan_dir = |dir: std::path::PathBuf, category: ModelCategory, exts: &[&'static str]|
        -> Vec<ModelRecord>
    {
        let mut found = vec![];
        let Ok(rd) = std::fs::read_dir(&dir) else { return found };
        for entry in rd.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if !exts.iter().any(|e| fname.ends_with(e)) { continue; }
            if known_filenames.contains(&fname) { continue; }
            let size_mb = entry.metadata().map(|m| m.len() / 1_048_576).unwrap_or(0);
            let name = fname
                .trim_end_matches(".gguf")
                .trim_end_matches(".llamafile")
                .trim_end_matches(".onnx")
                .trim_end_matches(".bin")
                .to_string();
            found.push(ModelRecord {
                id:              ModelRecord::id_for(&category, &name),
                category:        category.clone(),
                name,
                filename:        Some(fname),
                description:     "(detected on disk)".to_string(),
                size_mb,
                url:             None,
                hf_id:           None,
                ram_estimate_mb: None,
                recommended_role: None,
                context_length:  None,
                quantization:    None,
                asr_language:    None,
                asr_size:        None,
                tts_engine:      None,
                tts_voice_name:  None,
                config_filename: None,
                config_url:      None,
                tts_url:         None,
                sample_rate:     None,
                downloaded:      true,
                is_custom:       true,
            });
        }
        found
    };

    let mut extras = vec![];
    extras.extend(scan_dir(data_dir.join("models").join("gguf"),  ModelCategory::Gguf,      &[".gguf"]));
    extras.extend(scan_dir(data_dir.join("models").join("llm"),   ModelCategory::Llamafile, &[".llamafile", ".exe"]));
    extras.extend(scan_dir(data_dir.join("models"),               ModelCategory::Whisper,   &[".bin"]));
    extras.extend(scan_dir(data_dir.join("models").join("tts"),   ModelCategory::TtsPiper,  &[".onnx"]));

    // Persist newly discovered models to the catalog
    for m in &extras {
        let _ = model_repo.upsert(m).await;
    }

    extras
}

/// GET /api/v1/models — returns all catalog models with downloaded/active flags.
async fn list_models(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(model_repo) = &state.model_repo else {
        return Ok(Json(json!({"whisper": [], "llamafile": [], "tts": [], "gguf": []})));
    };

    // Discover any files on disk not yet in the catalog
    if let Some(data_dir) = &state.data_dir {
        let _ = scan_filesystem_extras(data_dir, model_repo).await;
    }

    let records = model_repo.list_all().await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    let assignments = model_repo.list_assignments().await.unwrap_or_default();

    let mut whisper   = vec![];
    let mut llamafile = vec![];
    let mut tts       = vec![];
    let mut gguf      = vec![];

    for m in &records {
        let v = serde_json::to_value(record_to_dto(m, &assignments)).unwrap_or_default();
        match m.category {
            ModelCategory::Whisper               => whisper.push(v),
            ModelCategory::Llamafile             => llamafile.push(v),
            ModelCategory::TtsPiper
            | ModelCategory::TtsHttp             => tts.push(v),
            ModelCategory::Gguf
            | ModelCategory::Ollama              => gguf.push(v),
        }
    }

    Ok(Json(json!({"whisper": whisper, "llamafile": llamafile, "tts": tts, "gguf": gguf})))
}

/// POST /api/v1/models/scan — explicit filesystem scan, persists and returns newly discovered entries.
async fn scan_models(
    State(state): State<Arc<AppState>>,
) -> Json<Value> {
    let (Some(data_dir), Some(model_repo)) = (&state.data_dir, &state.model_repo) else {
        return Json(json!({"found": 0, "entries": []}));
    };

    let extras = scan_filesystem_extras(data_dir, model_repo).await;
    let count  = extras.len();
    let assignments = model_repo.list_assignments().await.unwrap_or_default();
    let entries: Vec<Value> = extras.iter()
        .map(|m| serde_json::to_value(record_to_dto(m, &assignments)).unwrap_or_default())
        .collect();

    Json(json!({"found": count, "entries": entries}))
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

    let data_dir = state.model_storage_dir.clone().unwrap_or_else(|| std::path::PathBuf::from("."));

    // Fetch in the background so we don't block on slow network.
    tokio::spawn(async move {
        match catalog_provider.fetch().await {
            Ok((models, _binaries)) => {
                let count = models.len();
                for mut m in models {
                    // Update downloaded flag from disk.
                    m.downloaded = m.filename.as_ref()
                        .map(|f| match m.category {
                            pond_core::domain::model_record::ModelCategory::Whisper   => data_dir.join("models").join(f).exists(),
                            pond_core::domain::model_record::ModelCategory::Llamafile => data_dir.join("models").join("llm").join(f).exists(),
                            pond_core::domain::model_record::ModelCategory::Gguf      => data_dir.join("models").join("gguf").join(f).exists(),
                            pond_core::domain::model_record::ModelCategory::TtsPiper  => data_dir.join("models").join("tts").join(f).exists(),
                            _ => false,
                        })
                        .unwrap_or(matches!(m.category, pond_core::domain::model_record::ModelCategory::TtsHttp | pond_core::domain::model_record::ModelCategory::Ollama));
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
async fn get_download_progress(
    State(state): State<Arc<AppState>>,
) -> Json<Value> {
    let tracker = state.download_tracker.read().await;
    let entries: Vec<&DownloadEntry> = tracker.values().collect();
    Json(json!({"downloads": entries}))
}

/// POST /api/v1/models/{category}/{name}/download — trigger async model download.
async fn download_model(
    State(state): State<Arc<AppState>>,
    Path((category, name)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(model_repo) = state.model_repo.clone() else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "registry not available"}))));
    };
    let Some(data_dir) = state.data_dir.clone() else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "data_dir not configured"}))));
    };

    let cat = ModelCategory::from_str(&category)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Unknown category '{}'", category)}))))?;
    let model_id = ModelRecord::id_for(&cat, &name);

    let m = model_repo.get_by_id(&model_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(json!({"error": format!("Model '{}' not found in '{}'", name, category)}))))?;

    if m.downloaded {
        return Ok(Json(json!({"status": "already_downloaded", "name": name})));
    }
    let url = m.url.clone().ok_or_else(|| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": "model has no download URL"})))
    })?;
    let filename = m.filename.clone().ok_or_else(|| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": "model has no filename"})))
    })?;

    // Determine destination path based on category
    let dest = match cat {
        ModelCategory::Whisper   => data_dir.join("models").join(&filename),
        ModelCategory::Llamafile => data_dir.join("models").join("llm").join(&filename),
        ModelCategory::Gguf      => data_dir.join("models").join("gguf").join(&filename),
        ModelCategory::TtsPiper
        | ModelCategory::TtsHttp => data_dir.join("models").join("tts").join(&filename),
        ModelCategory::Ollama    => data_dir.join("models").join(&filename),
    };

    let tracker     = Arc::clone(&state.download_tracker);
    let dl_filename = filename.clone();
    let dl_category = category.clone();

    tokio::spawn(async move {
        spawn_tracked_download(url, dest, dl_filename, dl_category, tracker, async move {
            let _ = model_repo.set_downloaded(&model_id, true).await;
        }).await;
    });

    Ok(Json(json!({"status": "download_started", "name": name, "category": category})))
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
        return Err((StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "registry not available"}))));
    };

    let cat = ModelCategory::from_str(&category)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Unknown category '{}'", category)}))))?;
    let model_id = ModelRecord::id_for(&cat, &name);

    let m = model_repo.get_by_id(&model_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(json!({"error": format!("Model '{}' not found in '{}'", name, category)}))))?;

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
            ModelCategory::Whisper   => data_dir.join("models").join(filename),
            ModelCategory::Llamafile => data_dir.join("models").join("llm").join(filename),
            ModelCategory::Gguf      => data_dir.join("models").join("gguf").join(filename),
            ModelCategory::TtsPiper
            | ModelCategory::TtsHttp => data_dir.join("models").join("tts").join(filename),
            ModelCategory::Ollama    => data_dir.join("models").join(filename),
        };
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| {
                (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("Failed to delete file: {e}")})))
            })?;
        }
    }

    model_repo.set_downloaded(&model_id, false).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;

    Ok(StatusCode::NO_CONTENT)
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
        return Err((StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "registry not available"}))));
    };

    let Json(body) = body.map_err(|e| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Invalid JSON: {e}")})))
    })?;
    let role = body["role"].as_str().unwrap_or("").to_string();
    if role.is_empty() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "role is required"}))));
    }

    let cat = ModelCategory::from_str(&category)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Unknown category '{}'", category)}))))?;

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
    model_repo.get_by_id(&model_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(json!({"error": format!("Model '{}' not found in '{}'", name, category)}))))?;

    // Persist provider keys using runtime provider names (not category names).
    // GGUF category maps to the "local" provider in runtime routing.
    let provider = match cat {
        ModelCategory::Gguf      => "local",
        ModelCategory::Llamafile => "llamafile",
        ModelCategory::Ollama    => "ollama",
        ModelCategory::Whisper   => "asr",
        ModelCategory::TtsPiper  => "tts",
        ModelCategory::TtsHttp   => "tts",
    };

    // Persist assignment
    model_repo.set_assignment(&role, &model_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;

    // Sync to settings KV hot-cache
    let settings_repo = state.settings_repo.clone();
    match role.as_str() {
        "chat"  => {
            let _ = settings_repo.set_key("chat_model",    name.clone()).await;
            let _ = settings_repo.set_key("chat_provider", provider.to_string()).await;
        }
        "think" => {
            let _ = settings_repo.set_key("think_model",    name.clone()).await;
            let _ = settings_repo.set_key("think_provider", provider.to_string()).await;
        }
        "task"  => {
            let _ = settings_repo.set_key("task_model",    name.clone()).await;
            let _ = settings_repo.set_key("task_provider", provider.to_string()).await;
        }
        "asr"   => { let _ = settings_repo.set_key("active_whisper_model", name.clone()).await; }
        "tts"   => { let _ = settings_repo.set_key("active_tts_model",     name.clone()).await; }
        _       => {}
    }

    // Hot-rebuild the ModelRouter for LLM roles using the existing helper
    if matches!(role.as_str(), "chat" | "think" | "task") {
        let settings = state.settings_repo.get().await.unwrap_or_default();
        rebuild_model_router(&state, &settings).await;
    }

    Ok(Json(json!({"role": role, "model_id": model_id})))
}

/// GET /api/v1/models/ollama — proxy Ollama's /api/tags to list available local models.
/// Returns `{"models": [...]}` or `{"models": [], "error": "..."}` if Ollama is unreachable.
async fn list_ollama_models() -> Json<Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();
    match client.get("http://localhost:11434/api/tags").send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: Value = resp.json().await.unwrap_or(json!({"models": []}));
            Json(body)
        }
        Ok(resp) => Json(json!({"models": [], "error": format!("Ollama returned {}", resp.status())})),
        Err(_)   => Json(json!({"models": [], "error": "Ollama not running or not installed"})),
    }
}

/// POST /api/v1/models/ollama/pull — trigger `ollama pull <model>` on the server.
async fn pull_ollama_model(
    body: Result<Json<Value>, JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let model = match body {
        Ok(Json(v)) => v["model"].as_str().unwrap_or("").to_string(),
        Err(_) => return (StatusCode::BAD_REQUEST, Json(json!({"error": "expected {\"model\":\"name\"}"})))
    };
    if model.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "model name is required"})));
    }
    // Spawn `ollama pull <model>` as a background process (non-blocking).
    match tokio::process::Command::new("ollama")
        .args(["pull", &model])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_)  => (StatusCode::ACCEPTED, Json(json!({"status": "pulling", "model": model}))),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": format!("ollama not found: {e}")}))),
    }
}

/// GET /api/v1/models/search/gguf?q=<query> — proxy HuggingFace API for GGUF models.
async fn search_gguf_models(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let q = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let url = format!(
        "https://huggingface.co/api/models?filter=gguf&search={}&limit=20&sort=downloads&direction=-1",
        urlencoding::encode(q)
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default();
    match client.get(&url).send().await {
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
        Ok(resp) => Json(json!({"models": [], "error": format!("HuggingFace returned {}", resp.status())})),
        Err(e)   => Json(json!({"models": [], "error": format!("Request failed: {e}")})),
    }
}

/// GET /api/v1/models/search/llamafile?q=<query> — list llamafile releases from GitHub.
async fn search_llamafile_models(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let q = params.get("q").map(|s| s.to_lowercase()).unwrap_or_default();
    let url = "https://api.github.com/repos/Mozilla-Ocho/llamafile/releases?per_page=5";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default();
    match client.get(url).send().await {
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
        Ok(resp) => Json(json!({"models": [], "error": format!("GitHub returned {}", resp.status())})),
        Err(e)   => Json(json!({"models": [], "error": format!("Request failed: {e}")})),
    }
}

/// GET /api/v1/models/search/gguf/files?repo=<owner/name> — list .gguf files inside a HF repo.
async fn list_hf_model_files(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let repo = match params.get("repo") {
        Some(r) if !r.is_empty() => r.clone(),
        _ => return Json(json!({"files": [], "error": "repo param required"})),
    };
    // Do NOT percent-encode the repo — HF expects the literal owner/name path segment
    // (urlencoding::encode would turn '/' into '%2F' which returns 400)
    let url = format!("https://huggingface.co/api/models/{}", repo);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default();
    match client.get(&url).send().await {
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
        Ok(resp) => Json(json!({"files": [], "error": format!("HuggingFace returned {}", resp.status())})),
        Err(e)   => Json(json!({"files": [], "error": format!("Request failed: {e}")})),
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
        Err(_) => return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid JSON body"}))),
    };
    let url      = body["url"].as_str().unwrap_or("").to_string();
    let category = body["category"].as_str().unwrap_or("gguf").to_string();
    let filename = body["filename"].as_str().unwrap_or("").to_string();

    if url.is_empty() || filename.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "url and filename are required"})));
    }
    if !url.starts_with("https://") {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "only https URLs are accepted"})));
    }

    let Some(data_dir) = state.data_dir.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "data_dir not configured"})));
    };

    let dest = match category.as_str() {
        "whisper"   => data_dir.join("models").join(&filename),
        "llamafile" => data_dir.join("models").join("llm").join(&filename),
        "gguf"      => data_dir.join("models").join("gguf").join(&filename),
        "tts"       => data_dir.join("models").join("tts").join(&filename),
        _           => data_dir.join("models").join(&filename),
    };

    let tracker       = Arc::clone(&state.download_tracker);
    let resp_filename = filename.clone();
    let resp_category = category.clone();

    tokio::spawn(async move {
        spawn_tracked_download(url, dest, filename, category, tracker, async {}).await;
    });

    (StatusCode::ACCEPTED, Json(json!({"status": "downloading", "filename": resp_filename, "category": resp_category})))
}

/// Shared streaming download with progress tracking.
/// Streams the URL to `dest`, updating `tracker` as each chunk arrives.
/// Calls `on_done` (an async closure) when the download completes successfully.
async fn spawn_tracked_download<F>(
    url:      String,
    dest:     std::path::PathBuf,
    filename: String,
    category: String,
    tracker:  Arc<tokio::sync::RwLock<std::collections::HashMap<String, DownloadEntry>>>,
    on_done:  F,
) where F: std::future::Future<Output = ()> + Send {
    use tokio::io::AsyncWriteExt;

    // Register as in-progress
    {
        let mut t = tracker.write().await;
        t.insert(filename.clone(), DownloadEntry {
            filename:         filename.clone(),
            category:         category.clone(),
            downloaded_bytes: 0,
            total_bytes:      None,
            status:           "downloading".to_string(),
        });
    }

    if let Some(parent) = dest.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(7200))
        .build()
        .unwrap_or_default();

    tracing::info!("Downloading {} from {}", filename, url);

    let result: Result<(), String> = async {
        let resp = client.get(&url).send().await
            .map_err(|e| e.to_string())?;
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

        let mut file = tokio::fs::File::create(&dest).await
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
    }.await;

    match result {
        Ok(()) => {
            tracing::info!("Downloaded {} to {:?}", filename, dest);
            {
                let mut t = tracker.write().await;
                if let Some(e) = t.get_mut(&filename) {
                    e.status = "done".to_string();
                }
            }
            on_done.await;
        }
        Err(err) => {
            tracing::error!("Download {} failed: {}", filename, err);
            let mut t = tracker.write().await;
            if let Some(e) = t.get_mut(&filename) {
                e.status = "error".to_string();
            }
        }
    }
}


// ── Profile handlers ──────────────────────────────────────────────────────────

async fn list_profiles(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let profiles = state.profile_repo.list().await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    let list: Vec<Value> = profiles
        .iter()
        .map(|p| json!({
            "id":           p.id,
            "display_name": p.display_name,
            "avatar_emoji": p.avatar_emoji,
            "preferences":  p.preferences,
            "created_at":   p.created_at.to_rfc3339(),
            "updated_at":   p.updated_at.to_rfc3339(),
        }))
        .collect();
    Ok(Json(json!({ "profiles": list })))
}

async fn create_profile(
    State(state): State<Arc<AppState>>,
    body: Result<Json<CreateProfileRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Invalid request: {}", e)})))
    })?;
    let profile = state.profile_repo.create(req).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok((StatusCode::CREATED, Json(json!({
        "id":           profile.id,
        "display_name": profile.display_name,
        "avatar_emoji": profile.avatar_emoji,
        "preferences":  profile.preferences,
        "created_at":   profile.created_at.to_rfc3339(),
        "updated_at":   profile.updated_at.to_rfc3339(),
    }))))
}

async fn get_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let profile = state.profile_repo.get(&id).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
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
        None => Err((StatusCode::NOT_FOUND, Json(json!({"error": "profile not found"})))),
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
        (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Invalid request: {}", e)})))
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
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Speaker biometric handlers ────────────────────────────────────────────────

async fn enroll_speaker(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let speaker_id = state.speaker_id.as_ref().ok_or_else(|| (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "speaker identification not configured — run pond-server setup first"})),
    ))?;

    state.profile_repo.get(&profile_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(json!({"error": "profile not found"}))))?;

    let duration_secs = body
        .as_ref()
        .and_then(|b| b.get("duration_secs"))
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as u32;

    let audio = tokio::task::spawn_blocking(move || {
        pond_adapters_whisper::record_wav_sample(duration_secs)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;

    let embedding = speaker_id.register_speaker(&profile_id, &audio).await.map_err(|e| {
        (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"error": e.to_string()})))
    })?;

    let count = speaker_id.enrollment_count(&profile_id).await.unwrap_or(0);

    Ok(Json(json!({
        "embedding_id":   embedding.id,
        "profile_id":     embedding.profile_id,
        "model":          embedding.model,
        "dims":           embedding.dims,
        "enrolled_count": count,
        "created_at":     embedding.created_at.to_rfc3339(),
    })))
}

async fn delete_speaker_biometrics(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let speaker_id = state.speaker_id.as_ref().ok_or_else(|| (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "speaker identification not configured"})),
    ))?;
    speaker_id.delete_speaker(&profile_id).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Sensor handlers ───────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct SensorReadingRequest {
    device_id:   String,
    sensor_type: String,
    value:       f64,
    unit:        String,
}

async fn record_sensor(
    State(state): State<Arc<AppState>>,
    body: Result<Json<SensorReadingRequest>, JsonRejection>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Invalid request: {}", e)})))
    })?;
    let reading = SensorReading {
        device_id:   req.device_id,
        sensor_type: req.sensor_type,
        value:       req.value,
        unit:        req.unit,
        recorded_at: chrono::Utc::now(),
    };
    state.sensor_storage.record(reading).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok(StatusCode::CREATED)
}

#[derive(serde::Deserialize)]
struct SensorQueryParams {
    limit: Option<usize>,
}

async fn get_recent_sensors(
    State(state): State<Arc<AppState>>,
    Path(device_id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<SensorQueryParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let limit = params.limit.unwrap_or(20).min(100);
    let readings = state
        .sensor_storage
        .get_recent(&device_id, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    let list: Vec<Value> = readings
        .iter()
        .map(|r| json!({
            "device_id":   r.device_id,
            "sensor_type": r.sensor_type,
            "value":       r.value,
            "unit":        r.unit,
            "recorded_at": r.recorded_at.to_rfc3339(),
        }))
        .collect();
    Ok(Json(json!({ "readings": list })))
}

// ── Camera handlers ───────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct CameraEventRequest {
    camera_id:     String,
    event_type:    String,
    confidence:    Option<f64>,
    snapshot_path: Option<String>,
    metadata:      Option<String>,
}

#[derive(serde::Deserialize)]
struct CameraQueryParams {
    camera_id: Option<String>,
    limit:     Option<usize>,
}

async fn record_camera_event(
    State(state): State<Arc<AppState>>,
    body: Result<Json<CameraEventRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Invalid request: {}", e)})))
    })?;
    let event = CameraEvent {
        id:            None,
        camera_id:     req.camera_id,
        event_type:    req.event_type,
        confidence:    req.confidence,
        snapshot_path: req.snapshot_path,
        metadata:      req.metadata,
        acknowledged:  false,
        created_at:    chrono::Utc::now(),
    };
    let id = state.camera_storage.record_event(event).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    let list: Vec<Value> = events
        .iter()
        .map(|e| json!({
            "id":             e.id,
            "camera_id":      e.camera_id,
            "event_type":     e.event_type,
            "confidence":     e.confidence,
            "snapshot_path":  e.snapshot_path,
            "acknowledged":   e.acknowledged,
            "created_at":     e.created_at.to_rfc3339(),
        }))
        .collect();
    Ok(Json(json!({ "events": list })))
}

async fn acknowledge_camera_event(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state.camera_storage.acknowledge(id).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
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
            filename = field
                .file_name()
                .unwrap_or("audio.bin")
                .to_string();
            content_type = field
                .content_type()
                .unwrap_or("audio/wav")
                .to_string();
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
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": body})),
        ));
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
        (StatusCode::BAD_REQUEST, Json(json!({"error": format!("multipart error: {e}")})))
    })? {
        if field.name() == Some("audio") {
            filename = field.file_name().unwrap_or("audio.wav").to_string();
            content_type = field.content_type().unwrap_or("audio/wav").to_string();
            let bytes = field.bytes().await.map_err(|e| {
                (StatusCode::BAD_REQUEST, Json(json!({"error": format!("read error: {e}")})))
            })?;
            audio_bytes = Some(bytes.to_vec());
        }
    }

    let bytes = audio_bytes.ok_or_else(|| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": "missing 'audio' field in multipart body"})))
    })?;

    // ── Transcribe via whisper.cpp ───────────────────────────────────────────
    let whisper_url = format!("{}/inference", state.whisper_url);
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename)
        .mime_str(&content_type)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("MIME error: {e}")}))))?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "json");

    let resp = state.http_client.post(&whisper_url).multipart(form).send().await.map_err(|e| {
        (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("whisper server unreachable: {e}")})))
    })?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err((StatusCode::BAD_GATEWAY, Json(json!({"error": body}))));
    }

    let whisper_json: Value = resp.json().await.map_err(|e| {
        (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("whisper parse error: {e}")})))
    })?;

    let raw_transcript = whisper_json["text"].as_str().unwrap_or("").trim().to_string();
    if raw_transcript.is_empty() {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"error": "no speech detected in recording"}))));
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
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("settings load failed: {e}")})))
    })?;

    // Append only if this normalized variant is not already present.
    if !settings.voice_wake_word_transcriptions.contains(&normalized) {
        settings.voice_wake_word_transcriptions.push(normalized.clone());
    }

    let all_variants = settings.voice_wake_word_transcriptions.clone();
    let sample_count = all_variants.len();
    let complete     = sample_count >= TARGET_SAMPLES;

    state.settings_repo.update(&settings).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("settings save failed: {e}")})))
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
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("settings load failed: {e}")})))
    })?;

    settings.voice_wake_word_transcriptions.clear();

    state.settings_repo.update(&settings).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("settings save failed: {e}")})))
    })?;

    Ok(Json(json!({"cleared": true, "message": "Wake-word calibration data cleared"})))
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

/// `POST /api/v1/schedules` — create a new scheduled task.
async fn create_schedule(
    State(state): State<Arc<AppState>>,
    result: Result<Json<CreateTaskRequest>, JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    let Json(req) = match result {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))),
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

// ── Agent tools handler ───────────────────────────────────────────────────────

/// `GET /api/v1/agent/tools` — list all MCP tools currently loaded by the agent.
///
/// Returns a flat array of `{ extension, name, description }` objects.
/// Returns an empty array when no extension manager is active (no-crash fallback).
async fn list_agent_tools(
    State(state): State<Arc<AppState>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(manager) = &state.extension_manager else {
        return Json(json!([])).into_response();
    };
    match manager.list_tools().await {
        Ok(tools) => Json(json!(tools)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
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
    use pond_core::domain::agent::AgentRequest;
    use pond_core::ports::agent::AgentStreamEvent;
    use futures::stream::StreamExt;

    let body = match body {
        Ok(b) => b.0,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    let message = body["message"].as_str().unwrap_or("").to_string();
    let session_id = body["session_id"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let agent = state.agent.clone();

    let stream = async_stream::stream! {
        let request = AgentRequest {
            message,
            session_id: session_id.clone(),
            model_role: "task".to_string(),
        };

        let mut in_think_block = false; // filter <think> blocks
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
                    let data = match event {
                        AgentStreamEvent::Status { content } => {
                            json!({"type": "status", "content": content}).to_string()
                        }
                        AgentStreamEvent::ToolCall { tool, id, input } => {
                            json!({"type": "tool_call", "tool": tool, "id": id, "input": input}).to_string()
                        }
                        AgentStreamEvent::ToolResult { tool, id, content } => {
                            json!({"type": "tool_result", "tool": tool, "id": id, "content": content}).to_string()
                        }
                        AgentStreamEvent::Text { content } => {
                            let (visible, new_state) = pond_core::services::chat::filter_thinking(&content, in_think_block);
                            in_think_block = new_state;
                            if visible.is_empty() {
                                continue;
                            }
                            json!({"type": "text", "content": visible}).to_string()
                        }
                        AgentStreamEvent::Done { .. } => {
                            json!({"done": true, "session_id": session_id.clone()}).to_string()
                        }
                        AgentStreamEvent::Error { content } => {
                            json!({"error": content}).to_string()
                        }
                    };
                    yield Ok::<Event, std::convert::Infallible>(Event::default().data(data));
                }
                Err(e) => {
                    let data = json!({"error": e.to_string()}).to_string();
                    yield Ok(Event::default().data(data));
                    return;
                }
            }
        }
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

    let limit = params.get("limit")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(200)
        .min(2000);
    let level = params.get("level").map(|s| s.as_str());

    match repo.list(limit, level).await {
        Ok(entries) => Json(json!(entries)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ).into_response(),
    }
}

/// `GET /api/v1/logs/export` — download the event log as a CSV file.
///
/// Returns up to 10,000 rows across all severity levels.
async fn export_logs_csv(
    State(state): State<Arc<AppState>>,
) -> axum::response::Response {
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
        .header("Content-Disposition", "attachment; filename=\"pond-logs.csv\"")
        .body(Body::from(csv))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ── Extension management handlers ─────────────────────────────────────────────

/// `GET /api/v1/extensions` — list all active Goose/MCP extensions.
async fn list_extensions_handler(
    State(state): State<Arc<AppState>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(manager) = &state.extension_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Extension manager not available"})),
        )
            .into_response();
    };
    match manager.list_extensions().await {
        Ok(exts) => Json(json!({"extensions": exts})).into_response(),
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
    Json(req): Json<pond_core::ports::extension_manager::AddExtensionRequest>,
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
                let cfg = pond_core::ports::mcp_server::McpServerConfig {
                    id:          uuid::Uuid::new_v4().to_string(),
                    name:        req.name.clone(),
                    kind:        req.kind.clone(),
                    description: req.description.clone(),
                    command:     req.command.clone(),
                    args:        req.args.clone(),
                    env:         req.env.clone(),
                    uri:         req.uri.clone(),
                    enabled:     true,
                    created_at:  chrono::Utc::now().to_rfc3339(),
                };
                if let Err(e) = repo.save(&cfg).await {
                    tracing::warn!("Failed to persist MCP server '{}': {e}", req.name);
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
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
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
        ).into_response();
    };

    let body = match body {
        Ok(b) => b.0,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    let enabled = body["enabled"].as_bool().unwrap_or(true);

    match manager.set_enabled(&name, enabled).await {
        Ok(()) => Json(json!({"name": name, "enabled": enabled})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ).into_response(),
    }
}

// ── Prompt Templates ─────────────────────────────────────────────────────────

async fn list_prompt_templates(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_template_repo {
        Some(r) => r,
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Prompt template repository not configured"}))).into_response(),
    };
    match repo.list().await {
        Ok(templates) => Json(json!(templates)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

async fn get_prompt_template(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_template_repo {
        Some(r) => r,
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Prompt template repository not configured"}))).into_response(),
    };
    match repo.get(&name).await {
        Ok(Some(t)) => Json(json!(t)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"error": "Template not found"}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
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
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Prompt template repository not configured"}))).into_response(),
    };
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))).into_response(),
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
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

async fn delete_prompt_template(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_template_repo {
        Some(r) => r,
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Prompt template repository not configured"}))).into_response(),
    };
    // Don't allow deletion of system templates
    if let Ok(Some(t)) = repo.get(&name).await {
        if t.is_system {
            return (StatusCode::FORBIDDEN, Json(json!({"error": "Cannot delete built-in system templates"}))).into_response();
        }
    }
    match repo.delete(&name).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

// ── Prompt Extras ─────────────────────────────────────────────────────────────

async fn list_prompt_extras(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_extra_repo {
        Some(r) => r,
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Prompt extra repository not configured"}))).into_response(),
    };
    match repo.list_all().await {
        Ok(extras) => Json(json!(extras)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
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

fn bool_true() -> bool { true }

async fn upsert_prompt_extra(
    State(state): State<Arc<AppState>>,
    body: Result<Json<UpsertExtraRequest>, JsonRejection>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_extra_repo {
        Some(r) => r,
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Prompt extra repository not configured"}))).into_response(),
    };
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))).into_response(),
    };
    let extra = PromptExtra { key: req.key.clone(), instruction: req.instruction, active: req.active, sort_order: req.sort_order };
    match repo.upsert(&extra).await {
        Ok(()) => Json(json!({"key": req.key, "status": "ok"})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

async fn delete_prompt_extra(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.prompt_extra_repo {
        Some(r) => r,
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Prompt extra repository not configured"}))).into_response(),
    };
    match repo.delete(&key).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

// ── Memories ──────────────────────────────────────────────────────────────────

async fn list_memories(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    match state.memory_repo.search_recent(None, 50).await {
        Ok(memories) => Json(json!(memories)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

#[derive(Deserialize)]
struct SaveMemoryRequest {
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "default_source")]
    source: String,
}
fn default_source() -> String { "api".to_string() }

async fn save_memory(
    State(state): State<Arc<AppState>>,
    body: Result<Json<SaveMemoryRequest>, JsonRejection>,
) -> impl axum::response::IntoResponse {
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))).into_response(),
    };
    let fragment = MemoryFragment {
        id: Uuid::new_v4().to_string(),
        profile_id: None,
        session_id: None,
        content: req.content,
        embedding: None,
        source: req.source,
        tags: req.tags,
        created_at: chrono::Utc::now(),
    };
    match state.memory_repo.add(fragment.clone()).await {
        Ok(()) => (StatusCode::CREATED, Json(json!(fragment))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

async fn delete_memory(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl axum::response::IntoResponse {
    match state.memory_repo.delete(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

// ── Skills ────────────────────────────────────────────────────────────────────

async fn list_skills(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.skill_repo {
        Some(r) => r,
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Skill repository not configured"}))).into_response(),
    };
    match repo.list_all().await {
        Ok(skills) => Json(json!(skills)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
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
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Skill repository not configured"}))).into_response(),
    };
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))).into_response(),
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
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
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
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Skill repository not configured"}))).into_response(),
    };
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))).into_response(),
    };
    let existing = match repo.get(&id).await {
        Ok(Some(s)) => s,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "Skill not found"}))).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
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
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

async fn delete_skill(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.skill_repo {
        Some(r) => r,
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Skill repository not configured"}))).into_response(),
    };
    match repo.delete(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

// ── Recipes ───────────────────────────────────────────────────────────────────

async fn list_recipes(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.recipe_repo {
        Some(r) => r,
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Recipe repository not configured"}))).into_response(),
    };
    match repo.list().await {
        Ok(recipes) => Json(json!(recipes)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
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
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Recipe repository not configured"}))).into_response(),
    };
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))).into_response(),
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
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
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
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Recipe repository not configured"}))).into_response(),
    };
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))).into_response(),
    };
    let existing = match repo.get_by_id(&id).await {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "Recipe not found"}))).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
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
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

async fn delete_recipe(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl axum::response::IntoResponse {
    let repo = match &state.recipe_repo {
        Some(r) => r,
        None => return (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": "Recipe repository not configured"}))).into_response(),
    };
    match repo.delete(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}
