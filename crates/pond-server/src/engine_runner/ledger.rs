//! A ledger job: the Budget agent's maker that reads the payments one money picture shows, the
//! critic shown the same picture, and the entries that survive it. The picture and its capture
//! are deleted however the job ends.

use chrono::{DateTime, NaiveDate, Utc};

use pond_core::shared::domain::long_task::{ArtifactKind, ArtifactSummary, LongTask, PlanStep};
use pond_core::shared::domain::todo::{zone, Tz};
use pond_core::user_data::domain::capture::Capture;
use pond_core::user_data::domain::engine_job::{EngineJob, JobState};
use pond_core::user_data::domain::ledger::LedgerEntry;
use pond_core::user_data::services::engine::critic::Verdict;
use pond_core::user_data::services::engine::settle::job_outcome_line;
use pond_core::user_data::services::engine::steps::{
    critic_step, draft_artifact, verdict_artifact,
};
use pond_core::user_data::services::ledger_reading::{
    build_ledger_critic_message, parse_entries, parse_ledger_drops, LEDGER_CRITIC_SYSTEM,
    LEDGER_MESSAGE, LEDGER_SYSTEM,
};

use super::finish::{stage, why_no_draft, Stage};
use super::inline::{body_of, failed, Saved, StepCtx};
use super::pictures::{can_read, capture_of, close_and_forget_task, forget_capture, picture_of};
use super::{EngineDeps, ModelStop};

const DRAFT_TITLE: &str = "Draft";
const CHECK_TITLE: &str = "Check";
/// The pond keeps no household currency: an entry whose picture names none is left out, never
/// guessed.
const NO_CURRENCY: &str = "";
const GONE: &str = "Stopped: the picture was deleted.";
const WAITS: &str = "Failed: this pond cannot read pictures now; the picture waits until it can.";
const DELETED: &str = "The picture was deleted.";

/// The day the picture was taken, which an entry with no readable date falls on.
async fn day_of(deps: &EngineDeps, capture: &Capture) -> (NaiveDate, Tz) {
    let (timezone, _) = deps.clock_and_place().await;
    let tz = zone(&timezone);
    let taken = capture.taken_at.unwrap_or(capture.created_at);
    (taken.with_timezone(&tz).date_naive(), tz)
}

async fn entries_in(
    deps: &EngineDeps,
    raw: &str,
    capture: &Capture,
    now: DateTime<Utc>,
) -> Vec<LedgerEntry> {
    let (today, tz) = day_of(deps, capture).await;
    parse_entries(raw, &capture.profile_id, NO_CURRENCY, today, tz, now)
}

fn payments(count: usize) -> String {
    match count {
        0 => "no payment".to_string(),
        1 => "one payment".to_string(),
        count => format!("{count} payments"),
    }
}

/// The maker: the payments the picture shows the member made.
pub(super) async fn make(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let (capture, picture) = picture_of(ctx.deps, ctx.job).await?;
    let reply = ctx
        .ask_seeing(LEDGER_SYSTEM, LEDGER_MESSAGE.to_string(), picture)
        .await?;
    let read = entries_in(ctx.deps, &reply, &capture, ctx.now).await.len();
    let summary = format!("Read {}.", payments(read));
    let saved = ctx
        .save(ArtifactKind::Data, DRAFT_TITLE, &summary, reply)
        .await?;
    Ok(Saved {
        summary,
        artifacts: vec![saved],
    })
}

/// The critic: each entry checked against the same picture.
pub(super) async fn critic(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let (capture, picture) = picture_of(ctx.deps, ctx.job).await?;
    let (_, draft) = ctx.draft().await?;
    let entries = entries_in(ctx.deps, &draft, &capture, ctx.now).await;
    if entries.is_empty() {
        return Err(failed("the draft holds no payment to check"));
    }
    let reply = ctx
        .ask_seeing(
            LEDGER_CRITIC_SYSTEM,
            build_ledger_critic_message(&entries),
            picture,
        )
        .await?;
    let summary = match parse_ledger_drops(&reply, entries.len()) {
        None => "The check could not be read.".to_string(),
        Some(drops) => format!(
            "The check kept {} of {}.",
            entries.len() - drops.len(),
            payments(entries.len())
        ),
    };
    let saved = ctx
        .save(ArtifactKind::Data, CHECK_TITLE, &summary, reply)
        .await?;
    Ok(Saved {
        summary,
        artifacts: vec![saved],
    })
}

/// Reads the check's reply and saves the entries that survived it.
async fn save_checked(
    deps: &EngineDeps,
    task: &LongTask,
    job: &mut EngineJob,
    entries: Vec<LedgerEntry>,
    steps: &[PlanStep],
    artifacts: &[ArtifactSummary],
) -> anyhow::Result<(JobState, String)> {
    let reply = match verdict_artifact(steps, artifacts) {
        Some(found) => body_of(deps, task, found).await?,
        None => String::new(),
    };
    job.count_round(Vec::new());
    let Some(drops) = parse_ledger_drops(&reply, entries.len()) else {
        let said = job_outcome_line(job, Some(&Verdict::unread()));
        return Ok((JobState::Dropped, said));
    };
    let kept: Vec<LedgerEntry> = entries
        .into_iter()
        .enumerate()
        .filter(|(at, _)| !drops.contains(at))
        .map(|(_, entry)| entry)
        .collect();
    if kept.is_empty() {
        let said = "Dropped: the check kept no payment.".to_string();
        return Ok((JobState::Dropped, said));
    }
    let ledger = &deps
        .pictures
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("this pond keeps no ledger"))?
        .ledger;
    ledger.add(&kept).await?;
    let passed = Verdict {
        pass: true,
        objections: Vec::new(),
    };
    let said = format!(
        "{} Saved {}.",
        job_outcome_line(job, Some(&passed)),
        payments(kept.len())
    );
    Ok((JobState::Passed, said))
}

/// What follows a ledger task's steps: its one check, then the entries that survive it are
/// saved, and the picture is deleted whole whatever came of it.
pub(super) async fn after_steps(
    deps: &EngineDeps,
    task: &LongTask,
    mut job: EngineJob,
    steps: Vec<PlanStep>,
    artifacts: Vec<ArtifactSummary>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let Some(capture) = capture_of(deps, &job).await? else {
        return close_and_forget_task(deps, task, &mut job, JobState::Cancelled, GONE, now).await;
    };
    let at = stage(&steps);
    if matches!(at, Stage::NoDraft) && !can_read(deps).await {
        return close_and_forget_task(deps, task, &mut job, JobState::Failed, WAITS, now).await;
    }
    let entries = match draft_artifact(&steps, &artifacts).filter(|_| !matches!(at, Stage::NoDraft))
    {
        Some(found) => entries_in(deps, &body_of(deps, task, found).await?, &capture, now).await,
        None => Vec::new(),
    };
    let (state, said) = match at {
        Stage::NoDraft => (JobState::Failed, why_no_draft(&steps)),
        Stage::Unchecked if entries.is_empty() => (
            JobState::Dropped,
            "Dropped: the picture showed no payment it could read.".to_string(),
        ),
        Stage::Unchecked => match critic_step(task, &steps, &artifacts) {
            Ok(plan) => return deps.tasks.replace_open_steps(&plan).await,
            Err(_) => (JobState::Dropped, job_outcome_line(&job, None)),
        },
        Stage::CheckDied => (JobState::Dropped, job_outcome_line(&job, None)),
        Stage::Checked => save_checked(deps, task, &mut job, entries, &steps, &artifacts).await?,
    };
    // A money picture is deleted whole however its reading ended.
    forget_capture(deps, &capture).await?;
    tracing::info!(
        target: "giap::trace",
        kind = "engine_ledger_read",
        job = %job.id,
        state = state.as_str(),
        "a money picture was read and deleted"
    );
    let outcome = format!("{said} {DELETED}");
    close_and_forget_task(deps, task, &mut job, state, &outcome, now).await
}

/// A ledger task the runner gave up on: nothing is saved and the picture goes, unless the pond
/// cannot read pictures at all now.
pub(super) async fn out_of_road(
    deps: &EngineDeps,
    task: &LongTask,
    mut job: EngineJob,
    reason: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let (state, outcome) = match capture_of(deps, &job).await? {
        None => (JobState::Cancelled, GONE.to_string()),
        Some(_) if !can_read(deps).await => (JobState::Failed, WAITS.to_string()),
        Some(capture) => {
            forget_capture(deps, &capture).await?;
            (
                JobState::Failed,
                format!("Failed: {reason}. Nothing was saved. {DELETED}"),
            )
        }
    };
    close_and_forget_task(deps, task, &mut job, state, &outcome, now).await
}
