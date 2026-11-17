//! Opening the long task a queued job runs as.

use chrono::{DateTime, Utc};

use pond_core::shared::domain::long_task::LongTaskState;
use pond_core::shared::services::long_task_open::{open_task, OpeningBrief};
use pond_core::user_data::domain::engine_job::{is_engine_task, EngineJob, JobKind, JobState};
use pond_core::user_data::services::engine::task::{task_for_job, TaskRefused};

use super::{deliverable, EngineDeps};

const BRIEF_SUMMARY: &str = "What this is for and what is known about the member";

/// The goal of a job whose one maker step the code plans.
fn inline_goal(job: &EngineJob) -> String {
    match job.kind {
        JobKind::FactSheet => {
            "Write the fact sheet again from what is known about this member.".to_string()
        }
        JobKind::Questions => format!(
            "Write the questions to ask this member about {}.",
            job.subject
        ),
        JobKind::Heartbeat => "Review what this member may want done next.".to_string(),
        JobKind::Capture => "Read what one of this member's pictures shows.".to_string(),
        JobKind::Ledger => "Read the payments one of this member's pictures shows.".to_string(),
        JobKind::Rule => "Write one rule from what this member turned down.".to_string(),
        JobKind::Deliverable | JobKind::Memory | JobKind::Consolidation => job.subject.clone(),
    }
}

/// Opens `job`'s task and ties the two together. `Ok(false)` for a job that runs as no task.
async fn open(deps: &EngineDeps, job: &mut EngineJob, now: DateTime<Utc>) -> anyhow::Result<bool> {
    let owner = job
        .profile_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("the job names no member"))?;
    let making = match job.kind {
        JobKind::Deliverable => Some(deliverable::opening(deps, job, now).await?),
        _ => None,
    };
    let goal = match &making {
        Some(opening) => opening.goal.clone(),
        None => inline_goal(job),
    };
    let (task, plan) = match task_for_job(job, &goal, &owner, now) {
        Ok(made) => made,
        Err(TaskRefused::NotATask(_)) => return Ok(false),
        Err(refused) => return Err(refused.into()),
    };
    // The job names its task before the task is stored: the runner must never meet an engine
    // task that no job claims.
    let task_id = task.id().to_string();
    job.task_id = Some(task_id.clone());
    deps.jobs.save(job).await?;
    let opened = async {
        let brief = making.as_ref().map(|opening| OpeningBrief {
            summary: BRIEF_SUMMARY,
            body: opening.brief.clone(),
        });
        open_task(deps.tasks.as_ref(), task, brief).await?;
        if let Some(plan) = &plan {
            deps.tasks.replace_open_steps(plan).await?;
            deps.tasks
                .set_task_state(&task_id, LongTaskState::Running, now)
                .await?;
        }
        if let Some(opening) = making {
            let mut deliverable = opening.deliverable;
            // A refresh's task takes the place of the one that made the version before it.
            let before = deliverable.task_id.take().filter(|id| is_engine_task(id));
            deliverable.start_making(&task_id, now)?;
            deps.anchors.save_deliverable(&deliverable).await?;
            if let Some(before) = before {
                let _ = deps.tasks.delete_task(&before).await;
            }
            tracing::info!(
                target: "giap::trace",
                kind = "engine_deliverable_started",
                task_id = %task_id,
                anchor_kind = opening.anchor.kind.as_str(),
                slot = %deliverable.slot,
                version = deliverable.version,
                brief_chars = opening.brief.chars().count(),
                "a deliverable's research task opened"
            );
        }
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = opened {
        let _ = deps
            .tasks
            .set_task_state(&task_id, LongTaskState::Cancelled, now)
            .await;
        job.task_id = None;
        return Err(e);
    }
    deps.ring();
    Ok(true)
}

/// Opens a task for every queued job that has none; returns how many opened. A job that
/// cannot be opened is settled failed, so it is not tried again every minute.
pub(super) async fn open_queued(deps: &EngineDeps, now: DateTime<Utc>) -> usize {
    let waiting = match deps.jobs.open().await {
        Ok(jobs) => jobs,
        Err(e) => {
            tracing::warn!(error = %e, "the engine could not read its open jobs");
            return 0;
        }
    };
    let mut opened = 0;
    for mut job in waiting
        .into_iter()
        .filter(|job| job.state == JobState::Queued && job.task_id.is_none())
    {
        match open(deps, &mut job, now).await {
            Ok(true) => opened += 1,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "engine_job_not_opened",
                    job = %job.id,
                    job_kind = job.kind.as_str(),
                    error = %e,
                    "an engine job got no task"
                );
                deliverable::rest(deps, &job, now).await;
                if job
                    .settle(JobState::Failed, &format!("Failed: {e}."), now)
                    .is_ok()
                {
                    let _ = deps.jobs.save(&job).await;
                }
            }
        }
    }
    opened
}
