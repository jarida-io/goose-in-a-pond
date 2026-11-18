//! The heartbeat's intentions as their member reads them: what Goose means to do, once what
//! holds, until when. The member may withdraw one that is still waiting; carrying one out is
//! the heartbeat's.

use std::sync::Arc;

use axum::{
    extract::{rejection::QueryRejection, Path, Query, State},
    http::StatusCode,
    routing::{delete, get},
    Extension, Json, Router,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::intention::{Intention, IntentionState};

use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

pub(crate) const SERVED: bool = true;

/// Settled intentions one read lists, newest first.
const MAX_SETTLED: usize = 100;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/intentions", get(list))
        .route("/me/intentions/{id}", delete(withdraw))
}

fn view(intention: &Intention) -> Value {
    json!({
        "id": intention.id,
        "act": intention.act,
        "text": intention.text,
        "why": intention.why,
        "when": intention.condition_label,
        "until": intention.until,
        "state": intention.state,
        "outcome": intention.outcome,
        "anchor_id": intention.anchor_id,
        "created_at": intention.created_at,
        "settled_at": intention.settled_at,
    })
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default)]
    state: Option<String>,
}

async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Query(query) = query
        .map_err(|rejection| failure(StatusCode::UNPROCESSABLE_ENTITY, rejection.body_text()))?;
    let intentions = &state.engine.intentions;
    let found = match query.state.as_deref().map(str::trim) {
        None | Some("waiting") => intentions
            .list(&member, Some(IntentionState::Waiting))
            .await
            .map_err(server_error)?,
        Some("settled") => {
            let mut all = intentions.list(&member, None).await.map_err(server_error)?;
            all.retain(|intention| intention.state != IntentionState::Waiting);
            all.truncate(MAX_SETTLED);
            all
        }
        Some(_) => {
            return Err(failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "state must be waiting or settled",
            ))
        }
    };
    Ok(Json(json!({
        "intentions": found.iter().map(view).collect::<Vec<_>>(),
    })))
}

async fn withdraw(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let intentions = &state.engine.intentions;
    let no_such_intention = || failure(StatusCode::NOT_FOUND, "no such intention of yours");
    let mut intention = intentions
        .get(&member, &id)
        .await
        .map_err(server_error)?
        .ok_or_else(no_such_intention)?;
    match intention.state {
        IntentionState::Dropped => return Ok(StatusCode::NO_CONTENT),
        IntentionState::Waiting => {}
        IntentionState::Done | IntentionState::Expired => {
            return Err(failure(
                StatusCode::CONFLICT,
                "this is already over, so there is nothing left to withdraw",
            ))
        }
    }
    intention.withdraw(Utc::now()).map_err(server_error)?;
    if !intentions.save(&intention).await.map_err(server_error)? {
        return Err(no_such_intention());
    }
    Ok(StatusCode::NO_CONTENT)
}
