//! What the runner does with an engine task where it would finish, plan or give up on any
//! other: the engine decides what follows the steps, and settles the job.

use chrono::{DateTime, Utc};

use pond_core::shared::domain::long_task::{
    ArtifactSummary, LongTask, LongTaskState, PlanStep, StepState,
};
use pond_core::user_data::domain::engine_job::{EngineJob, JobKind, JobState};
use pond_core::user_data::services::engine::critic::{parse_verdict, Verdict};
use pond_core::user_data::services::engine::steps::{
    critic_step, draft_artifact, latest_critic_step, latest_draft_step, next_after_steps,
    revise_step, verdict_artifact, Next,
};
use pond_core::user_data::services::engine::ROLE_CRITIC;

use super::inline::{body_of, CANNOT_RUN};
use super::{
    capture, cards, deliverable, fact_sheet, is_failed_inline, ledger, plans_itself, questions,
    rule, EngineDeps,
};

/// How far a job of one maker step and one check has got.
pub(super) enum Stage {
    NoDraft,
    Unchecked,
    /// A check was added and never finished.
    CheckDied,
    Checked,
}

pub(super) fn stage(steps: &[PlanStep]) -> Stage {
    let Some(draft) = latest_draft_step(steps) else {
        return Stage::NoDraft;
    };
    if latest_critic_step(steps).is_some() {
        return Stage::Checked;
    }
    let asked = steps
        .iter()
        .any(|step| step.role() == ROLE_CRITIC && step.ordinal() > draft.ordinal());
    if asked {
        Stage::CheckDied
    } else {
        Stage::Unchecked
    }
}

/// The ruling on the draft as it stands, read from what its check saved.
pub(super) async fn ruling(
    deps: &EngineDeps,
    task: &LongTask,
    steps: &[PlanStep],
    artifacts: &[ArtifactSummary],
) -> anyhow::Result<Option<Verdict>> {
    Ok(match verdict_artifact(steps, artifacts) {
        Some(found) => Some(parse_verdict(&body_of(deps, task, found).await?)),
        None => None,
    })
}

/// Where the critic loop of a job that settles on a verdict stands after its steps.
pub(super) enum Loop {
    /// A check or a correction was added: the task goes on.
    Added,
    Settle(JobState, Option<Verdict>),
}

/// One turn of the critic loop: counts the round a new ruling makes, then adds the check or
/// the correction that comes next, or says how the job settles.
pub(super) async fn critic_loop(
    deps: &EngineDeps,
    task: &LongTask,
    job: &mut EngineJob,
    steps: &[PlanStep],
    artifacts: &[ArtifactSummary],
    now: DateTime<Utc>,
) -> anyhow::Result<Loop> {
    let verdict = ruling(deps, task, steps, artifacts).await?;
    let checked = steps
        .iter()
        .filter(|step| step.role() == ROLE_CRITIC && step.state() == StepState::Completed)
        .count();
    if let Some(ruled) = verdict.as_ref().filter(|_| (job.rounds as usize) < checked) {
        job.count_round(ruled.objections.clone());
        deps.jobs.save(job).await?;
    }
    let next = match draft_artifact(steps, artifacts) {
        Some(_) => next_after_steps(job, steps, verdict.as_ref(), now),
        None => Next::Settle(JobState::Failed),
    };
    let added = match next {
        Next::Settle(state) => return Ok(Loop::Settle(state, verdict)),
        Next::AddCritic => critic_step(task, steps, artifacts),
        Next::AddRevise => revise_step(task, steps, artifacts),
    };
    match added {
        Ok(plan) => {
            deps.tasks.replace_open_steps(&plan).await?;
            Ok(Loop::Added)
        }
        Err(refused) => {
            tracing::warn!(task = %task.id(), reason = %refused, "the engine could not add a step");
            Ok(Loop::Settle(job.kind.settles_without_a_pass_as(), verdict))
        }
    }
}

/// Why a job's steps produced nothing, in the words its last failed step left.
pub(super) fn why_no_draft(steps: &[PlanStep]) -> String {
    let reason = steps
        .iter()
        .rev()
        .find_map(|step| step.failure())
        .unwrap_or("its step did not run");
    format!("Failed: {reason}.")
}

/// Writes how a job ended and ends its task with it.
pub(super) async fn close(
    deps: &EngineDeps,
    task: &LongTask,
    job: &mut EngineJob,
    state: JobState,
    outcome: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    job.settle(state, outcome, now)?;
    deps.jobs.save(job).await?;
    let ended = match state {
        JobState::Failed => LongTaskState::Failed,
        JobState::Cancelled => LongTaskState::Cancelled,
        _ => LongTaskState::Completed,
    };
    if let Err(e) = deps.tasks.set_task_state(task.id(), ended, now).await {
        tracing::warn!(task = %task.id(), error = %e, "a settled job's task could not be ended");
    }
    tracing::info!(
        target: "giap::trace",
        kind = "engine_job_settled",
        job = %job.id,
        task_id = %task.id(),
        job_kind = job.kind.as_str(),
        state = state.as_str(),
        rounds = job.rounds,
        "an engine job settled"
    );
    Ok(())
}

/// The task's open job; a task whose job is gone or already settled is ended here.
async fn open_job(
    deps: &EngineDeps,
    task: &LongTask,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<EngineJob>> {
    let job = deps.jobs.by_task(task.id()).await?;
    if let Some(job) = job.filter(|job| job.state.is_open()) {
        return Ok(Some(job));
    }
    if !task.state().is_terminal() {
        let _ = deps
            .tasks
            .set_task_state(task.id(), LongTaskState::Cancelled, now)
            .await;
    }
    Ok(None)
}

/// Decides what follows an engine task's steps once they are all settled: another step, or
/// the settle. `Ok(true)`: the task moved.
pub async fn at_finish(
    deps: &EngineDeps,
    task: &LongTask,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let Some(mut job) = open_job(deps, task, now).await? else {
        return Ok(true);
    };
    let steps = deps.tasks.list_steps(task.id()).await?;
    let artifacts = deps.tasks.list_artifacts(task.id()).await?;
    match job.kind {
        JobKind::Deliverable => {
            deliverable::after_steps(deps, task, job, steps, artifacts, now).await?
        }
        JobKind::FactSheet => {
            fact_sheet::after_steps(deps, task, job, steps, artifacts, now).await?
        }
        JobKind::Questions => {
            questions::after_steps(deps, task, job, steps, artifacts, now).await?
        }
        JobKind::Capture => capture::after_steps(deps, task, job, steps, artifacts, now).await?,
        JobKind::Ledger => ledger::after_steps(deps, task, job, steps, artifacts, now).await?,
        JobKind::Rule => rule::after_steps(deps, task, job, steps, artifacts, now).await?,
        JobKind::Cards => cards::after_steps(deps, task, job, steps, artifacts, now).await?,
        _ => {
            let outcome = format!("Failed: {CANNOT_RUN}.");
            close(deps, task, &mut job, JobState::Failed, &outcome, now).await?
        }
    }
    Ok(true)
}

/// Settles the job of an engine task the runner gives up on, and ends the task.
pub async fn give_up(
    deps: &EngineDeps,
    task: &LongTask,
    reason: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let Some(mut job) = open_job(deps, task, now).await? else {
        return Ok(());
    };
    match job.kind {
        JobKind::Deliverable => {
            return deliverable::out_of_road(deps, task, job, reason, now).await
        }
        JobKind::Capture => return capture::out_of_road(deps, task, job, reason, now).await,
        JobKind::Ledger => return ledger::out_of_road(deps, task, job, reason, now).await,
        _ => {}
    }
    let outcome = format!("Failed: {reason}.");
    close(deps, task, &mut job, JobState::Failed, &outcome, now).await
}

/// What the engine does where the runner would hand a task to the planner. `None` leaves it
/// to the planner: a deliverable's research with no inline step in its way.
pub async fn instead_of_planning(
    deps: &EngineDeps,
    task: &LongTask,
    steps: &[PlanStep],
    now: DateTime<Utc>,
) -> anyhow::Result<Option<bool>> {
    let Some(mut job) = open_job(deps, task, now).await? else {
        return Ok(Some(true));
    };
    if !plans_itself(Some(job.kind), steps) {
        return Ok(None);
    }
    // Skipped, the steps are all settled, and the finish hook decides what the job comes to.
    let mut skipped = false;
    for mut step in steps.iter().filter(|s| is_failed_inline(s)).cloned() {
        if step.skip(now).is_ok() {
            deps.tasks.update_step(&step).await?;
            skipped = true;
        }
    }
    if !skipped {
        let outcome = if steps.is_empty() {
            "Failed: its plan was lost."
        } else {
            "Failed: its steps could not go on."
        };
        close(deps, task, &mut job, JobState::Failed, outcome, now).await?;
    }
    Ok(Some(true))
}
