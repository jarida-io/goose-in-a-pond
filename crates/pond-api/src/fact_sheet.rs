//! A member's fact sheet and don't-list: what Goose keeps about them, and what they told it
//! not to offer. The member reads, corrects and removes lines here; a rebuild is only asked
//! for, and the engine's runner does it.

use std::sync::{Arc, LazyLock};

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    routing::{delete, get, patch, post},
    Extension, Json, Router,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::dont_list::{DontRule, RuleOrigin, Strength};
use pond_core::user_data::domain::engine_job::{EngineJob, JobClass};
use pond_core::user_data::domain::fact_sheet::{
    clean_fact, fact_key, FactCategory, FactLine, FactPatch, FactState, FACT_SHEET_LIMIT,
};
use pond_core::user_data::services::fact_sheet;

use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

/// Serialises "is a rebuild already open?" with queueing one, so two asks queue one job.
static REBUILD_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/facts", get(list_facts).post(add_fact))
        .route("/me/facts/rebuild", post(rebuild))
        .route("/me/facts/{id}", patch(change_fact).delete(remove_fact))
        .route("/me/dont", get(list_rules).post(add_rule))
        .route("/me/dont/{id}", delete(remove_rule))
}

fn bad_body(rejection: JsonRejection) -> Failure {
    failure(StatusCode::UNPROCESSABLE_ENTITY, rejection.body_text())
}

fn no_such_fact() -> Failure {
    failure(StatusCode::NOT_FOUND, "no such fact")
}

async fn sheet(state: &AppState, member: &str) -> Result<Json<Value>, Failure> {
    let facts = state
        .engine
        .facts
        .list(member, FactState::Active)
        .await
        .map_err(server_error)?;
    Ok(Json(json!({ "facts": facts, "limit": FACT_SHEET_LIMIT })))
}

async fn list_facts(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    sheet(&state, &member).await
}

#[derive(Debug, Deserialize)]
struct NewFact {
    category: FactCategory,
    fact: String,
}

async fn add_fact(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<NewFact>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(body) = body.map_err(bad_body)?;
    let now = Utc::now();
    let Some(line) = FactLine::written_by_member(&member, body.category, &body.fact, now) else {
        return Err(failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "a fact needs some words",
        ));
    };
    let facts = &state.engine.facts;
    let held = facts
        .by_key(&member, &line.key())
        .await
        .map_err(server_error)?;
    match held {
        // A removed line is closed to a rebuild, not to the member: they may write it again,
        // and the rule its removal left goes with it.
        Some(removed) if removed.state == FactState::Removed => {
            let revived = FactLine {
                id: removed.id,
                created_at: removed.created_at,
                used: removed.used,
                ..line
            };
            facts.save(&revived).await.map_err(server_error)?;
            let dont = &state.engine.dont;
            for rule in dont.list(&member, None).await.map_err(server_error)? {
                if rule.origin == RuleOrigin::Fact
                    && rule.origin_ref.as_deref() == Some(revived.id.as_str())
                {
                    dont.delete(&member, &rule.id).await.map_err(server_error)?;
                }
            }
        }
        _ => {
            facts.write(&line).await.map_err(server_error)?;
        }
    }
    sheet(&state, &member).await
}

async fn change_fact(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Result<Json<FactPatch>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(patch) = body.map_err(bad_body)?;
    let facts = &state.engine.facts;
    let mut line = facts
        .get(&member, &id)
        .await
        .map_err(server_error)?
        .filter(|line| line.state == FactState::Active)
        .ok_or_else(no_such_fact)?;
    if let Some(reworded) = patch.fact.as_deref() {
        let Some(reworded) = clean_fact(reworded) else {
            return Err(failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "a fact needs some words",
            ));
        };
        let other = facts
            .by_key(&member, &fact_key(&reworded))
            .await
            .map_err(server_error)?
            .filter(|other| other.id != line.id);
        if let Some(other) = other {
            let why = if other.state == FactState::Removed {
                "you removed a line that says that: add it again instead"
            } else {
                "another line of your fact sheet already says that"
            };
            return Err(failure(StatusCode::CONFLICT, why));
        }
    }
    patch.apply(&mut line, Utc::now());
    if !facts.save(&line).await.map_err(server_error)? {
        return Err(no_such_fact());
    }
    Ok(Json(json!(line)))
}

/// Marks the line removed, so no rebuild learns it again, and keeps it on the don't-list so
/// nothing is built on it either.
async fn remove_fact(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let facts = &state.engine.facts;
    let line = facts
        .get(&member, &id)
        .await
        .map_err(server_error)?
        .filter(|line| line.state != FactState::Removed)
        .ok_or_else(no_such_fact)?;
    let now = Utc::now();
    // The rule first: a second try after a failed removal merges into it.
    if let Some(mut rule) =
        DontRule::new(&member, &line.fact, Strength::Avoid, RuleOrigin::Fact, now)
    {
        rule.origin_ref = Some(line.id.clone());
        state.engine.dont.add(&rule).await.map_err(server_error)?;
    }
    if !facts
        .remove(&member, &id, now)
        .await
        .map_err(server_error)?
    {
        return Err(no_such_fact());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Queues a rebuild of the member's sheet, or finds the one already open.
async fn ask_for_rebuild(
    state: &AppState,
    member: &str,
    class: JobClass,
) -> anyhow::Result<EngineJob> {
    let _held = REBUILD_LOCK.lock().await;
    fact_sheet::request_rebuild(state.engine.jobs.as_ref(), member, class, Utc::now()).await
}

/// A rebuild after an interview changed what the sheet is built from; best effort.
pub(crate) async fn rebuild_after_interview(state: &AppState, member: &str) {
    if let Err(e) = ask_for_rebuild(state, member, JobClass::Upkeep).await {
        tracing::warn!(error = %e, "a fact sheet rebuild could not be queued");
    }
}

async fn rebuild(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let job = ask_for_rebuild(&state, &member, JobClass::Asked)
        .await
        .map_err(server_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "state": job.state.as_str() })),
    ))
}

async fn rules(state: &AppState, member: &str) -> Result<Json<Value>, Failure> {
    let rules = state
        .engine
        .dont
        .list(member, None)
        .await
        .map_err(server_error)?;
    Ok(Json(json!({ "rules": rules })))
}

async fn list_rules(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    rules(&state, &member).await
}

#[derive(Debug, Deserialize)]
struct NewRule {
    rule: String,
    /// A rule a member writes by hand is general unless they say otherwise.
    #[serde(default)]
    strength: Option<Strength>,
}

async fn add_rule(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<NewRule>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(body) = body.map_err(bad_body)?;
    let strength = body.strength.unwrap_or(Strength::Never);
    let Some(rule) = DontRule::new(&member, &body.rule, strength, RuleOrigin::You, Utc::now())
    else {
        return Err(failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "a rule needs some words",
        ));
    };
    state.engine.dont.add(&rule).await.map_err(server_error)?;
    rules(&state, &member).await
}

async fn remove_rule(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    if !state
        .engine
        .dont
        .delete(&member, &id)
        .await
        .map_err(server_error)?
    {
        return Err(failure(StatusCode::NOT_FOUND, "no such rule"));
    }
    rules(&state, &member).await
}
