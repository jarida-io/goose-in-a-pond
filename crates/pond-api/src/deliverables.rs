//! What Goose made for an anchor, as a member reads it: open it, keep it as a file, ask for it
//! again, or turn it down with a reason that goes on their don't-list.

use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::anchor::{Anchor, AnchorState, Deliverable, DeliverableState};
use pond_core::user_data::domain::dont_list::{Reason, RuleOrigin, Strength};
use pond_core::user_data::domain::engine_job::{EngineJob, JobClass, JobKind};

use crate::anchors::{bad_body, members_words, not_yours, turned_down};
use crate::engine_log::{ask_for_refresh, call_off, job_summary};
use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/deliverables/{id}", get(read))
        .route("/me/deliverables/{id}/refresh", post(refresh))
        .route("/me/deliverables/{id}/reject", post(reject))
        .route("/me/deliverables/{id}/seen", post(seen))
        .route("/me/deliverables/{id}/file", get(file))
}

fn no_such_deliverable() -> Failure {
    failure(StatusCode::NOT_FOUND, "no such deliverable")
}

/// A deliverable the caller may read, with the anchor it was made for.
async fn readable(
    state: &AppState,
    member: &str,
    id: &str,
) -> Result<(Deliverable, Anchor), Failure> {
    let anchors = &state.engine.anchors;
    let deliverable = anchors
        .deliverable(member, id)
        .await
        .map_err(server_error)?
        .ok_or_else(no_such_deliverable)?;
    let anchor = anchors
        .get(member, &deliverable.anchor_id)
        .await
        .map_err(server_error)?
        .ok_or_else(no_such_deliverable)?;
    Ok((deliverable, anchor))
}

/// A deliverable of an anchor the caller owns, so may change.
async fn owned(state: &AppState, member: &str, id: &str) -> Result<(Deliverable, Anchor), Failure> {
    let found = readable(state, member, id).await?;
    if found.1.profile_id != member {
        return Err(not_yours());
    }
    Ok(found)
}

async fn read(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let (deliverable, anchor) = readable(&state, &member, &id).await?;
    let job = state
        .engine
        .jobs
        .open_for_deliverable(&deliverable.id)
        .await
        .map_err(server_error)?;
    let mine = anchor.profile_id == member;
    Ok(Json(json!({
        "id": deliverable.id,
        "anchor_id": anchor.id,
        "anchor_title": anchor.title,
        "kind": deliverable.kind,
        "slot": deliverable.slot,
        "title": deliverable.title,
        "state": deliverable.state,
        "hook": deliverable.hook,
        "body": deliverable.body,
        "facts": deliverable.facts,
        "sources": deliverable.sources,
        "objections": deliverable.objections,
        "changed": deliverable.changed,
        "version": deliverable.version,
        "task_id": deliverable.task_id,
        "refresh_at": deliverable.refresh_at,
        "unseen": mine && deliverable.is_unseen(),
        "mine": mine,
        "created_at": deliverable.created_at,
        "updated_at": deliverable.updated_at,
        "job": job.as_ref().map(job_summary),
    })))
}

async fn refresh(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let (deliverable, anchor) = owned(&state, &member, &id).await?;
    let refused = match (anchor.state, deliverable.state) {
        (_, DeliverableState::Rejected) => Some("you turned this down, so it is not made again"),
        (AnchorState::Active, state) if state != DeliverableState::Archived => None,
        _ => Some("what this was made for is over, so it is no longer kept up to date"),
    };
    if let Some(why) = refused {
        return Err(failure(StatusCode::CONFLICT, why));
    }
    let job = ask_for_refresh(&state, &anchor, &deliverable, Utc::now())
        .await
        .map_err(server_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "state": job.state.as_str() })),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Verb {
    NotThis,
    Never,
}

#[derive(Debug, Deserialize)]
struct Rejection {
    verb: Verb,
    #[serde(default)]
    reasons: Vec<Reason>,
    #[serde(default)]
    text: Option<String>,
}

/// Queues the job that rewrites the member's words into one general rule, once per rule.
async fn ask_for_general_rule(state: &AppState, member: &str, rule_id: &str) -> anyhow::Result<()> {
    let jobs = &state.engine.jobs;
    let asked = jobs
        .open_of_kind(member, JobKind::Rule)
        .await?
        .iter()
        .any(|job| job.subject == rule_id);
    if !asked {
        let job = EngineJob::new(
            JobKind::Rule,
            JobClass::Upkeep,
            Some(member),
            rule_id,
            Utc::now(),
        );
        jobs.add(&job).await?;
    }
    Ok(())
}

async fn reject(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Result<Json<Rejection>, JsonRejection>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(body) = body.map_err(bad_body)?;
    let (mut deliverable, _) = owned(&state, &member, &id).await?;
    if deliverable.state == DeliverableState::Rejected {
        return Ok(StatusCode::NO_CONTENT);
    }
    // A refresh under way still shows the last version, which is what the member turns down.
    let refreshing = deliverable.state == DeliverableState::Making && deliverable.version > 0;
    if !deliverable.state.is_shown() && !refreshing {
        return Err(failure(
            StatusCode::CONFLICT,
            "there is nothing here to turn down yet",
        ));
    }
    let now = Utc::now();
    let said = members_words(body.text.as_deref());
    let strength = match body.verb {
        Verb::NotThis => Strength::Avoid,
        Verb::Never => Strength::Never,
    };
    let rule_for = |what: &str| {
        let from = (RuleOrigin::Reject, deliverable.id.as_str());
        turned_down(
            &member,
            what,
            strength,
            &body.reasons,
            said.clone(),
            from,
            now,
        )
    };
    // A "never" is the member's own words until the engine writes the general rule; with no
    // words, or words that hold no rule, it is the title.
    let rule = said
        .as_deref()
        .filter(|_| body.verb == Verb::Never)
        .and_then(rule_for)
        .or_else(|| rule_for(&deliverable.title));
    // The rule first: a second try after a failed rejection merges into it.
    let kept = match rule {
        Some(rule) => Some(state.engine.dont.add(&rule).await.map_err(server_error)?),
        None => None,
    };

    let anchors = &state.engine.anchors;
    let jobs = &state.engine.jobs;
    if let Some(job) = jobs
        .open_for_deliverable(&deliverable.id)
        .await
        .map_err(server_error)?
    {
        call_off(&state, job, "Turned down by the member.", now)
            .await
            .map_err(server_error)?;
        deliverable = anchors
            .find_deliverable(&deliverable.id)
            .await
            .map_err(server_error)?
            .ok_or_else(no_such_deliverable)?;
    }
    deliverable
        .reject(now)
        .map_err(|_| failure(StatusCode::CONFLICT, "this changed just now: open it again"))?;
    if !anchors
        .save_deliverable(&deliverable)
        .await
        .map_err(server_error)?
    {
        return Err(no_such_deliverable());
    }

    if let (Verb::Never, Some(_), Some(rule)) = (body.verb, &said, &kept) {
        ask_for_general_rule(&state, &member, &rule.id)
            .await
            .map_err(server_error)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn seen(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let (mut deliverable, anchor) = readable(&state, &member, &id).await?;
    // The mark is its owner's: a member it is shared with reads it without clearing it.
    if anchor.profile_id == member && deliverable.seen_at.is_none() {
        deliverable.mark_seen(Utc::now());
        state
            .engine
            .anchors
            .save_deliverable(&deliverable)
            .await
            .map_err(server_error)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn file(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let (deliverable, _) = readable(&state, &member, &id).await?;
    if deliverable.body.trim().is_empty() {
        return Err(failure(
            StatusCode::NOT_FOUND,
            "this has not been written yet: try again when it is ready",
        ));
    }
    let name = crate::report_export::file_stem(&deliverable.title);
    Ok((
        [
            (
                header::CONTENT_TYPE,
                "text/markdown; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}.md\""),
            ),
        ],
        deliverable.body,
    )
        .into_response())
}
