//! Ideas Goose researched for a member, which Goose or the member can act on. The lane writes
//! them (`pond-server`'s `my_goose_mind`); these routes read them and start the member's choice.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::Utc;
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::idea::{GooseKind, Idea, IdeaState};
use pond_core::user_data::ports::ideas::{IdeaRepository, IdeaTodo};
use pond_core::user_data::services::inference_lane::LaneJob;

use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

/// Whether this Pond serves this part of My Goose; `/me/hub` reports it.
pub(crate) const SERVED: bool = true;

/// When a scheduled idea runs: Monday morning in the household's own time zone.
const WEEKLY_CRON: &str = "0 0 8 * * MON";

pub(crate) fn repo(state: &AppState) -> Arc<dyn IdeaRepository> {
    Arc::new(pond_infra::sqlite_ideas::SqliteIdeaRepository::new(
        state.db.system.clone(),
    ))
}

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/ideas", get(list))
        .route("/me/ideas/refresh", post(refresh))
        .route("/me/ideas/{id}/goose", post(goose))
        .route("/me/ideas/{id}/mine", post(mine))
        .route("/me/ideas/{id}/dismiss", post(dismiss))
}

async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let repo = repo(&state);
    let ideas = repo.list(&member).await.map_err(server_error)?;
    let run = repo.run(&member).await.map_err(server_error)?;
    Ok(Json(json!({
        "ideas": ideas,
        "state": if run.is_thinking(Utc::now()) { "thinking" } else { "idle" },
    })))
}

async fn refresh(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Response, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    repo(&state)
        .mark_requested(&member, Utc::now())
        .await
        .map_err(server_error)?;
    // Without a lane (CLI, tests) the request waits for the next process that has one.
    if let Some(lane) = &state.lane {
        lane.wake(LaneJob::Ideas).await;
    }
    Ok((StatusCode::ACCEPTED, Json(json!({ "state": "thinking" }))).into_response())
}

async fn dismiss(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    match repo(&state)
        .set_state(&member, &id, IdeaState::Dismissed, None)
        .await
    {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(failure(StatusCode::NOT_FOUND, "no such idea")),
        Err(e) => Err(server_error(e)),
    }
}

async fn the_idea(state: &AppState, member: &str, id: &str) -> Result<Idea, Failure> {
    repo(state)
        .get(member, id)
        .await
        .map_err(server_error)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such idea"))
}

/// A to-do as `/me/todos` shows one; its hooks are the to-do routes' business.
fn todo_json(todo: &IdeaTodo) -> Value {
    json!({
        "id": todo.id,
        "kind": "todo",
        "text": todo.text,
        "notes": null,
        "state": "open",
        "due_at": null,
        "hooks": [],
        "ask": true,
        "source": "idea",
        "last_asked_at": null,
        "asked": 0,
        "snoozed_until": null,
        "created_at": todo.created_at,
        "done_at": null,
    })
}

/// `POST /me/ideas/{id}/mine`: each step the idea suggests becomes an open to-do.
async fn mine(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let idea = the_idea(&state, &member, &id).await?;
    let repo = repo(&state);
    let steps: Vec<String> = if idea.you_can.is_empty() {
        vec![idea.title.clone()]
    } else {
        idea.you_can.clone()
    };
    let mut todos = Vec::new();
    for step in &steps {
        let todo = repo
            .add_todo(&member, &idea.id, step)
            .await
            .map_err(server_error)?;
        todos.push(todo_json(&todo));
    }
    if idea.state == IdeaState::New {
        repo.set_state(&member, &idea.id, IdeaState::Working, None)
            .await
            .map_err(server_error)?;
    }
    let idea = the_idea(&state, &member, &id).await?;
    Ok(Json(json!({ "idea": idea, "todos": todos })))
}

/// `POST /me/ideas/{id}/goose`: Goose starts its part and the idea moves to `working`.
async fn goose(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let idea = the_idea(&state, &member, &id).await?;
    if idea.state == IdeaState::Working {
        return Ok(Json(json!(idea)));
    }
    let Some(part) = idea.goose_can.clone() else {
        return Err(failure(
            StatusCode::CONFLICT,
            "Goose has nothing to do for this idea",
        ));
    };
    let (state_to_set, task_id) = match part.kind {
        // The phone opens a chat about it; nothing starts on the Pond.
        GooseKind::Chat => return Ok(Json(json!(idea))),
        GooseKind::Research => (
            IdeaState::Working,
            Some(start_research(&state, &member, &idea).await?),
        ),
        GooseKind::Schedule => {
            start_schedule(&state, &idea).await?;
            (IdeaState::Done, None)
        }
        GooseKind::Todo => {
            let first = idea
                .you_can
                .first()
                .cloned()
                .unwrap_or_else(|| idea.title.clone());
            repo(&state)
                .add_todo(&member, &idea.id, &first)
                .await
                .map_err(server_error)?;
            (IdeaState::Working, None)
        }
        GooseKind::Watch => {
            start_watch(&state, &member, &idea).await?;
            (IdeaState::Done, None)
        }
    };
    repo(&state)
        .set_state(&member, &idea.id, state_to_set, task_id.as_deref())
        .await
        .map_err(server_error)?;
    tracing::info!(
        target: "giap::trace",
        kind = "idea_started",
        part = ?part.kind,
        "Goose started its part of an idea"
    );
    Ok(Json(json!(the_idea(&state, &member, &id).await?)))
}

/// A deep-research long task the member owns, reported into a conversation of its own.
async fn start_research(state: &AppState, member: &str, idea: &Idea) -> Result<String, Failure> {
    use pond_core::shared::services::long_task_open::open_task;
    use pond_core::user_data::domain::session::{IdentificationSource, SessionIdentity};

    let settings = state.settings_repo.get().await.unwrap_or_default();
    if !(settings.ext_orchestrator_enabled && settings.long_tasks_enabled) {
        return Err(failure(
            StatusCode::CONFLICT,
            "deep research is switched off on this Pond",
        ));
    }
    let question = idea.goose_plan.prompt.clone().unwrap_or_else(|| {
        format!(
            "{}: {}. Research practical execution steps, requirements, and resources for this household on Goose In A Pond.",
            idea.title, idea.pitch
        )
    });
    let session_id = uuid::Uuid::new_v4().to_string();
    state
        .session_storage
        .create_session(session_id.clone())
        .await
        .map_err(server_error)?;
    let _ = state
        .session_storage
        .update_title(&session_id, idea.title.clone())
        .await;
    let _ = state
        .session_storage
        .set_session_identity(
            &session_id,
            &SessionIdentity {
                profile_id: Some(member.to_string()),
                source: IdentificationSource::Explicit,
                confidence: None,
            },
        )
        .await;
    let task = pond_core::shared::services::deep_research::research_task(
        member,
        &question,
        &session_id,
        Utc::now(),
    )
    .map_err(server_error)?;
    let repo = pond_infra::sqlite_long_task::SqliteLongTaskRepository::new(state.db.system.clone());
    let task = open_task(&repo, task, None).await.map_err(server_error)?;
    Ok(task.id().to_string())
}

/// A weekly prompt Goose runs, from the idea's own question.
async fn start_schedule(state: &AppState, idea: &Idea) -> Result<(), Failure> {
    use pond_core::user_data::domain::schedule::TaskKind;
    use pond_core::user_data::ports::scheduler::CreateScheduleRequest;

    let Some(scheduler) = &state.scheduler else {
        return Err(failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "this Pond has no scheduler running",
        ));
    };
    let settings = state.settings_repo.get().await.unwrap_or_default();
    let prompt = idea
        .goose_plan
        .prompt
        .clone()
        .unwrap_or_else(|| idea.title.clone());
    scheduler
        .create_task(CreateScheduleRequest {
            id: uuid::Uuid::new_v4().to_string(),
            label: idea.title.chars().take(80).collect(),
            cron: WEEKLY_CRON.to_string(),
            fire_at: None,
            once: false,
            timezone: settings.timezone,
            kind: TaskKind::AgentPrompt { prompt },
            ask_if_missed: false,
        })
        .await
        .map_err(server_error)?;
    Ok(())
}

/// A web watch on the listings page the idea names; its first check sets the baseline.
async fn start_watch(state: &AppState, member: &str, idea: &Idea) -> Result<(), Failure> {
    use pond_core::shared::domain::web_watch::{
        WatchFilters, DEFAULT_INTERVAL_HOURS, MAX_WATCHES_PER_MEMBER,
    };
    use pond_core::shared::ports::web_watch::{WebWatch, WebWatchRepository};

    let Some(address) = idea.goose_plan.listing_url.clone() else {
        return Err(failure(
            StatusCode::CONFLICT,
            "this idea names no page of listings to watch",
        ));
    };
    let repo = pond_infra::sqlite_web_watch::SqliteWebWatchRepository::new(state.db.system.clone());
    let mine = repo.list_for(member).await.map_err(server_error)?;
    if mine.iter().any(|w| w.address == address) {
        return Ok(());
    }
    if mine.len() >= MAX_WATCHES_PER_MEMBER {
        return Err(failure(
            StatusCode::CONFLICT,
            format!("you already keep {MAX_WATCHES_PER_MEMBER} watches; stop one first"),
        ));
    }
    let now = Utc::now();
    repo.create(&WebWatch {
        id: uuid::Uuid::new_v4().to_string(),
        owner: member.to_string(),
        address,
        title: idea.title.clone(),
        looking_for: idea.pitch.chars().take(300).collect(),
        filters: WatchFilters::default(),
        interval_hours: DEFAULT_INTERVAL_HOURS,
        paused: false,
        items: None,
        failures: 0,
        last_checked_at: None,
        next_check_at: now,
        created_at: now,
        session_id: None,
    })
    .await
    .map_err(server_error)?;
    Ok(())
}
