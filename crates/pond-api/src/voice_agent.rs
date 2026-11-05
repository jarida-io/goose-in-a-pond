//! `GET/PUT/DELETE /api/v1/agents/voice`: the shipped Voice agent and the household's one edit of
//! it, kept as an `agent_recipes` row that `/recipes` never lists.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::rejection::{ExtensionRejection, JsonRejection};
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use pond_core::shared::domain::orchestration::AgentRole;
use pond_core::shared::domain::role_block::FieldError;
use pond_core::shared::ports::agent_prefill::AgentPrefillRegistry as _;
pub(crate) use pond_core::shared::services::voice_agent::is_voice_row;
use pond_core::shared::services::voice_agent::{
    voice_agent_from_row, voice_tool_groups, VoiceAgent, VoicePatch, VOICE_AGENT,
    VOICE_AGENT_TITLE, VOICE_TEMPLATE,
};
use pond_core::user_data::domain::recipe::AgentRecipe;
use serde_json::{json, Value};

use crate::routes::{acting_member, on_this_host, prefill_row};
use crate::AppState;

type Principal = Option<Extension<pond_core::security::ports::policy::Principal>>;
type Peer = Result<ConnectInfo<std::net::SocketAddr>, ExtensionRejection>;
type Insecure = Option<Extension<crate::insecure_dev::InsecureDevTransport>>;
type Tailnet = Option<Extension<pond_core::security::ports::remote_access::TailnetMachine>>;

/// How often a waiting compile looks for a quiet moment; a turn is seconds long.
const QUIET_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// The active prompt style's `<voice-mode>` text, read where a voice turn reads its template.
async fn shipped_instructions(state: &AppState) -> String {
    let style = state
        .settings_repo
        .get()
        .await
        .map(|s| s.prompt_style)
        .unwrap_or_default();
    let stored = match &state.prompt_template_repo {
        Some(repo) => repo.get(&style).await.ok().flatten().map(|t| t.content),
        None => None,
    };
    let template = stored.unwrap_or_else(|| {
        pond_core::prompts::builtin_template_content("balanced")
            .map(|(content, _)| content)
            .unwrap_or(pond_core::prompts::PROMPT_BALANCED)
            .to_string()
    });
    pond_core::prompts::voice_mode_body(&template)
        .unwrap_or_default()
        .to_string()
}

async fn stored_row(state: &AppState) -> Result<Option<AgentRecipe>, Response> {
    let Some(repo) = &state.recipe_repo else {
        return Err(no_store());
    };
    repo.get_by_name(VOICE_AGENT)
        .await
        .map_err(|e| failed(&e.to_string()))
}

/// The Voice agent's role for the prefill registry, in the shape serve's prewarm compiles.
pub(crate) async fn voice_prefill_role(state: &AppState) -> Option<AgentRole> {
    let row = match &state.recipe_repo {
        Some(repo) => repo.get_by_name(VOICE_AGENT).await.ok().flatten(),
        None => None,
    };
    let agent = voice_agent_from_row(row.as_ref()).unwrap_or_default();
    agent.prefill_role(&shipped_instructions(state).await)
}

async fn prefill_of(state: &AppState, role: &AgentRole) -> Value {
    use pond_core::shared::services::prefill_progress::process_prefill_progress;
    let progress = process_prefill_progress();
    let retention = state
        .settings_repo
        .get()
        .await
        .map(|s| s.agent_prefill_retention_days)
        .unwrap_or(14);
    let records =
        pond_infra::sqlite_agent_prefill::SqliteAgentPrefillRegistry::new(state.db.system.clone())
            .get_all()
            .await
            .unwrap_or_default();
    prefill_row(
        role,
        records.get(role.name()),
        &progress.build(),
        &progress.pending_rebuilds(),
        &progress.failed(),
        chrono::Utc::now(),
        retention,
    )
}

async fn body(state: &AppState, agent: &VoiceAgent) -> Value {
    let shipped = shipped_instructions(state).await;
    let shipped_max_turns = state
        .settings_repo
        .get()
        .await
        .map(|s| s.voice_max_turns)
        .ok();
    let tool_groups: Vec<Value> = voice_tool_groups()
        .into_iter()
        .map(|(group, label)| json!({"group": group, "label": label, "on": agent.keeps_group(group)}))
        .collect();
    let prefill = match agent.prefill_role(&shipped) {
        Some(role) => prefill_of(state, &role).await,
        None => Value::Null,
    };
    json!({
        "name": VOICE_AGENT,
        "title": VOICE_AGENT_TITLE,
        "edited": agent.is_edited(),
        "instructions": agent.effective_instructions(&shipped),
        "shipped_instructions": shipped,
        "examples": agent.examples,
        "tool_groups": tool_groups,
        "max_turns": agent.max_turns,
        "thinking": agent.thinking.as_str(),
        "shipped_max_turns": shipped_max_turns,
        "prefill": prefill,
    })
}

/// `GET /api/v1/agents/voice`
pub(crate) async fn get_voice_agent(State(state): State<Arc<AppState>>) -> Response {
    let row = match stored_row(&state).await {
        Ok(row) => row,
        Err(response) => return response,
    };
    let agent = voice_agent_from_row(row.as_ref()).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "the Voice agent's edit does not read; showing the shipped one");
        VoiceAgent::default()
    });
    Json(body(&state, &agent).await).into_response()
}

/// `PUT /api/v1/agents/voice`: the household's edit, by a member. Each field left out keeps what
/// is there; an edit that matches the shipped agent removes the row.
pub(crate) async fn put_voice_agent(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    peer: Peer,
    insecure: Insecure,
    tailnet: Tailnet,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    let Some(member) = member(&state, principal, peer, insecure, tailnet).await else {
        return guest();
    };
    let payload = match payload {
        Ok(Json(value)) => value,
        Err(e) => return invalid(vec![field_error("", &e.body_text())]),
    };
    let patch = match VoicePatch::from_json(&payload) {
        Ok(patch) => patch,
        Err(errors) => return invalid(errors),
    };
    let row = match stored_row(&state).await {
        Ok(row) => row,
        Err(response) => return response,
    };
    if let Some(other) = row.as_ref().filter(|r| !is_voice_row(r)) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": format!(
                "a household agent is already named `{}`; rename it to edit the Voice agent",
                other.name
            )})),
        )
            .into_response();
    }
    // An edit that no longer reads starts again from the shipped agent rather than failing.
    let current = voice_agent_from_row(row.as_ref()).unwrap_or_default();
    let next = match current.patched(&patch) {
        Ok(next) => next,
        Err(errors) => return invalid(errors),
    };
    if let Err(response) = store(&state, row, &next, &member).await {
        return response;
    }
    tracing::info!(
        member = %member,
        edited = next.is_edited(),
        "the Voice agent was saved; its prefill recompiles"
    );
    if next != current {
        spawn_voice_prefill(state.clone());
    }
    Json(body(&state, &next).await).into_response()
}

/// `DELETE /api/v1/agents/voice`: back to the shipped agent.
pub(crate) async fn reset_voice_agent(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    peer: Peer,
    insecure: Insecure,
    tailnet: Tailnet,
) -> Response {
    let Some(member) = member(&state, principal, peer, insecure, tailnet).await else {
        return guest();
    };
    let row = match stored_row(&state).await {
        Ok(row) => row,
        Err(response) => return response,
    };
    if let Some(row) = row.filter(is_voice_row) {
        if let Err(response) = store(&state, Some(row), &VoiceAgent::default(), &member).await {
            return response;
        }
        tracing::info!(member = %member, "the Voice agent was reset; its prefill recompiles");
        spawn_voice_prefill(state.clone());
    }
    Json(body(&state, &VoiceAgent::default()).await).into_response()
}

async fn member(
    state: &Arc<AppState>,
    principal: Principal,
    peer: Peer,
    insecure: Insecure,
    tailnet: Tailnet,
) -> Option<String> {
    let on_host = on_this_host(peer.ok(), insecure.is_some(), tailnet.is_some());
    acting_member(state, principal.as_ref(), on_host).await
}

/// Writes `agent` as the row, or removes the row when `agent` is the shipped one.
async fn store(
    state: &AppState,
    row: Option<AgentRecipe>,
    agent: &VoiceAgent,
    member: &str,
) -> Result<(), Response> {
    let Some(repo) = &state.recipe_repo else {
        return Err(no_store());
    };
    if !agent.is_edited() {
        if let Some(row) = row {
            repo.delete(&row.id)
                .await
                .map_err(|e| failed(&e.to_string()))?;
        }
        return Ok(());
    }
    let recipe = AgentRecipe {
        id: row
            .as_ref()
            .map(|r| r.id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        name: VOICE_AGENT.to_string(),
        description: "The household's edit of the Voice agent.".to_string(),
        yaml: agent.to_recipe_yaml(),
        active: true,
        created_at: row
            .as_ref()
            .map(|r| r.created_at.clone())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
        owner_profile_id: Some(member.to_string()),
        template: Some(VOICE_TEMPLATE.to_string()),
    };
    repo.upsert(&recipe)
        .await
        .map_err(|e| failed(&e.to_string()))
}

/// One Voice agent compile at a time; saves that land while one waits share it.
#[derive(Default)]
pub struct VoicePrefillQueue {
    waiting: Arc<AtomicBool>,
    compiling: tokio::sync::Mutex<()>,
}

/// Marks a compile as waiting until it drops, so a cancelled wait never blocks later compiles.
struct Waiting(Arc<AtomicBool>);

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Compiles the Voice agent's prompt shape once no turn is being answered, then records it in the
/// prefill registry. Saves that land while one compile waits share it.
pub async fn compile_voice_prefill(state: Arc<AppState>) {
    let queue = &state.runs.voice_prefill;
    if queue.waiting.swap(true, Ordering::SeqCst) {
        return;
    }
    let waiting = Waiting(queue.waiting.clone());
    let _compiling = queue.compiling.lock().await;
    while state.runs.turn_in_flight() {
        tokio::time::sleep(QUIET_POLL).await;
    }
    // A save from here on needs its own compile: this one may already have read the old row.
    drop(waiting);
    let Some(role) = voice_prefill_role(&state).await else {
        return;
    };
    // Lane jobs wait for it as for a turn.
    let _turn = state.runs.turn_started();
    let ready = Arc::new(AtomicBool::new(false));
    let saw_ready = ready.clone();
    let progress = Arc::new(move |phase: pond_core::models::ports::agent::WarmupPhase| {
        if matches!(phase, pond_core::models::ports::agent::WarmupPhase::Ready) {
            saw_ready.store(true, Ordering::SeqCst);
        }
    });
    state.agent.prewarm(true, progress).await;
    if !ready.load(Ordering::SeqCst) {
        return;
    }
    let hash = pond_core::shared::services::agent_prefill::definition_hash(&role);
    let registry =
        pond_infra::sqlite_agent_prefill::SqliteAgentPrefillRegistry::new(state.db.system.clone());
    match registry
        .note_compiled(VOICE_AGENT, &hash, chrono::Utc::now())
        .await
    {
        Ok(()) => tracing::info!(
            target: "giap::trace",
            kind = "voice_prefill_compiled",
            agent = VOICE_AGENT,
            "the Voice agent's prefill is compiled"
        ),
        Err(e) => {
            tracing::debug!(error = %e, "the prefill registry did not record the Voice agent's compile")
        }
    }
}

/// [`compile_voice_prefill`] in the background.
pub fn spawn_voice_prefill(state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(compile_voice_prefill(state))
}

fn field_error(path: &str, message: &str) -> FieldError {
    FieldError {
        path: path.to_string(),
        message: message.to_string(),
    }
}

fn invalid(errors: Vec<FieldError>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"errors": errors})),
    )
        .into_response()
}

fn guest() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error": "only a household member can change the Voice agent"})),
    )
        .into_response()
}

fn no_store() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({"error": "Recipe repository not configured"})),
    )
        .into_response()
}

fn failed(error: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": error})),
    )
        .into_response()
}
