//! The idea engine's jobs as a member's routes reach them: the log the member reads, how a
//! route asks for a refresh and how it calls a job off. A route never runs a job; the engine's
//! runner does.

use std::sync::Arc;

use axum::{
    extract::{rejection::QueryRejection, Query, State},
    http::StatusCode,
    routing::get,
    Extension, Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::shared::domain::long_task::LongTaskState;
use pond_core::shared::ports::long_task::LongTaskRepository;
use pond_core::shared::services::long_task_roles::{
    COMPOSER_ROLE, PLANNER_ROLE, VERIFIER_ROLE, WORKER_ROLE,
};
use pond_core::user_data::domain::anchor::{Anchor, Deliverable};
use pond_core::user_data::domain::engine_job::{EngineJob, JobClass, JobKind, JobState};
use pond_core::user_data::services::engine::refresh::request_refresh;
use pond_core::user_data::services::engine::{ROLE_CRITIC, ROLE_MAKE, ROLE_REVISE};
use pond_infra::sqlite_long_task::SqliteLongTaskRepository;

use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

const DEFAULT_JOBS: usize = 30;
const MAX_JOBS: usize = 100;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/me/engine/jobs", get(recent_jobs))
}

fn long_tasks(state: &AppState) -> SqliteLongTaskRepository {
    SqliteLongTaskRepository::new(state.db.system.clone())
}

fn kind_in_words(kind: JobKind) -> &'static str {
    match kind {
        JobKind::Deliverable => "Something made for you",
        JobKind::FactSheet => "Your fact sheet",
        JobKind::Memory => "Memories",
        JobKind::Consolidation => "A tidy-up of memories",
        JobKind::Heartbeat => "A check-in",
        JobKind::Capture => "A picture",
        JobKind::Ledger => "A payment record",
        JobKind::Rule => "A rule",
        JobKind::Questions => "Questions for you",
    }
}

/// What a job is about, in words: a capture, ledger or rule job keeps an id as its subject.
fn subject_of(job: &EngineJob) -> String {
    let named = match job.kind {
        JobKind::Capture | JobKind::Ledger | JobKind::Rule => None,
        _ => Some(job.subject.trim())
            .filter(|subject| !subject.is_empty() && uuid::Uuid::parse_str(subject).is_err()),
    };
    named.unwrap_or(kind_in_words(job.kind)).to_string()
}

/// An open job as the anchor or deliverable it works for shows it.
pub(crate) fn job_summary(job: &EngineJob) -> Value {
    json!({
        "kind": job.kind,
        "subject": subject_of(job),
        "state": job.state,
        "rounds": job.rounds,
    })
}

/// The open job a surface shows for an anchor: the one running, else the next in the queue.
pub(crate) fn shown_job<'a>(open: &'a [EngineJob], anchor_id: &str) -> Option<&'a EngineJob> {
    let of_anchor = || {
        open.iter()
            .filter(|job| job.anchor_id.as_deref() == Some(anchor_id))
    };
    of_anchor()
        .find(|job| job.state == JobState::Running)
        .or_else(|| of_anchor().next())
}

fn step_label(role: &str) -> &'static str {
    match role.trim() {
        ROLE_MAKE => "Writing a draft",
        ROLE_CRITIC => "Checking the draft",
        ROLE_REVISE => "Revising",
        PLANNER_ROLE => "Planning the work",
        WORKER_ROLE => "Researching",
        VERIFIER_ROLE => "Checking the findings",
        COMPOSER_ROLE => "Writing it up",
        _ => "Working on it",
    }
}

/// The steps of a job's long task, each under a label a member can read; none before the
/// runner has opened the task.
pub(crate) async fn job_steps(state: &AppState, job: &EngineJob) -> anyhow::Result<Vec<Value>> {
    let Some(task_id) = job.task_id.as_deref() else {
        return Ok(Vec::new());
    };
    let steps = long_tasks(state).list_steps(task_id).await?;
    Ok(steps
        .iter()
        .map(|step| {
            json!({
                "key": step.key().as_str(),
                "role": step.role(),
                "label": step_label(step.role()),
                "state": step.state(),
                "summary": step.summary(),
            })
        })
        .collect())
}

/// Queues a refresh the member asked for, or finds the deliverable's open job and moves it
/// ahead of the engine's own work.
pub(crate) async fn ask_for_refresh(
    state: &AppState,
    anchor: &Anchor,
    deliverable: &Deliverable,
    now: DateTime<Utc>,
) -> anyhow::Result<EngineJob> {
    request_refresh(
        state.engine.jobs.as_ref(),
        anchor,
        deliverable,
        JobClass::Asked,
        now,
    )
    .await
}

/// Settles an open job as cancelled, stops its long task as a member's cancel does, and hands
/// a deliverable it was making back to what the member last had.
pub(crate) async fn call_off(
    state: &AppState,
    mut job: EngineJob,
    why: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    if job.settle(JobState::Cancelled, why, now).is_err() {
        return Ok(());
    }
    state.engine.jobs.save(&job).await?;
    if let Some(task_id) = job.task_id.as_deref() {
        let tasks = long_tasks(state);
        let running = tasks
            .get_task(task_id)
            .await?
            .is_some_and(|task| !task.state().is_terminal());
        if running {
            // Refused only when the runner finished the task first, which stops it as well.
            if let Err(e) = tasks
                .set_task_state(task_id, LongTaskState::Cancelled, now)
                .await
            {
                tracing::warn!(error = %e, task_id, "an engine task could not be cancelled");
            }
        }
    }
    if let Some(deliverable_id) = job.deliverable_id.as_deref() {
        let anchors = &state.engine.anchors;
        if let Some(mut deliverable) = anchors.find_deliverable(deliverable_id).await? {
            if deliverable.release(now).is_ok() {
                anchors.save_deliverable(&deliverable).await?;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct LogQuery {
    #[serde(default)]
    limit: Option<usize>,
}

fn log_line(job: &EngineJob) -> Value {
    json!({
        "id": job.id,
        "kind": job.kind,
        "subject": subject_of(job),
        "state": job.state,
        "rounds": job.rounds,
        "objections": job.objections,
        "outcome": job.outcome,
        "anchor_id": job.anchor_id,
        "deliverable_id": job.deliverable_id,
        "created_at": job.created_at,
        "started_at": job.started_at,
        "finished_at": job.finished_at,
    })
}

async fn recent_jobs(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    query: Result<Query<LogQuery>, QueryRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Query(query) = query
        .map_err(|rejection| failure(StatusCode::UNPROCESSABLE_ENTITY, rejection.body_text()))?;
    let limit = query.limit.unwrap_or(DEFAULT_JOBS).clamp(1, MAX_JOBS);
    let jobs = state
        .engine
        .jobs
        .recent(&member, limit)
        .await
        .map_err(server_error)?;
    Ok(Json(
        json!({ "jobs": jobs.iter().map(log_line).collect::<Vec<_>>() }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(kind: JobKind, subject: &str) -> EngineJob {
        EngineJob::new(kind, JobClass::Upkeep, Some("jerry"), subject, Utc::now())
    }

    #[test]
    fn a_job_is_named_in_words_and_never_by_an_id() {
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(subject_of(&job(JobKind::Capture, &id)), "A picture");
        assert_eq!(subject_of(&job(JobKind::Ledger, &id)), "A payment record");
        assert_eq!(subject_of(&job(JobKind::Rule, &id)), "A rule");
        assert_eq!(
            subject_of(&job(JobKind::Questions, &id)),
            "Questions for you"
        );
        assert_eq!(subject_of(&job(JobKind::Heartbeat, "  ")), "A check-in");
        assert_eq!(
            subject_of(&job(JobKind::Deliverable, "Dossier for AI Summit Nairobi")),
            "Dossier for AI Summit Nairobi"
        );
        assert_eq!(
            subject_of(&job(JobKind::FactSheet, "Fact sheet")),
            "Fact sheet"
        );
    }

    #[test]
    fn every_step_has_a_label_a_member_can_read() {
        assert_eq!(step_label(ROLE_MAKE), "Writing a draft");
        assert_eq!(step_label(ROLE_CRITIC), "Checking the draft");
        assert_eq!(step_label(ROLE_REVISE), "Revising");
        for role in [
            PLANNER_ROLE,
            WORKER_ROLE,
            VERIFIER_ROLE,
            COMPOSER_ROLE,
            "new:scout",
        ] {
            let label = step_label(role);
            assert!(
                !label.is_empty() && !label.contains(role),
                "{role}: {label}"
            );
        }
    }

    #[test]
    fn an_anchor_shows_its_running_job_before_a_queued_one() {
        let of = |anchor: &str, state: JobState| {
            let mut job = job(JobKind::Deliverable, "A plan");
            job.anchor_id = Some(anchor.to_string());
            job.state = state;
            job
        };
        let open = [
            of("trip", JobState::Queued),
            of("summit", JobState::Queued),
            of("trip", JobState::Running),
        ];
        assert_eq!(shown_job(&open, "trip").unwrap().id, open[2].id);
        assert_eq!(shown_job(&open, "summit").unwrap().id, open[1].id);
        assert!(shown_job(&open, "car").is_none());
    }
}
