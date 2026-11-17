//! A questions job: the maker that writes what to ask a member about an anchor, the critic
//! that drops what is already settled, and the batch that is stored and pushed.

use chrono::{DateTime, Utc};

use pond_core::shared::domain::long_task::{ArtifactKind, ArtifactSummary, LongTask, PlanStep};
use pond_core::user_data::domain::anchor::Anchor;
use pond_core::user_data::domain::engine_job::{EngineJob, JobState};
use pond_core::user_data::domain::fact_sheet::FactState;
use pond_core::user_data::services::engine::critic::Verdict;
use pond_core::user_data::services::engine::questions::{
    build_questions_critic_message, build_questions_message, parse_question_drops, parse_questions,
    without_dropped, QUESTIONS_CRITIC_SYSTEM, QUESTIONS_SYSTEM,
};
use pond_core::user_data::services::engine::settle::job_outcome_line;
use pond_core::user_data::services::engine::steps::{
    critic_step, draft_artifact, verdict_artifact,
};
use pond_core::user_data::services::fact_sheet::{sheet_for_brief, MAX_ANSWERS};

use super::finish::{close, stage, why_no_draft, Stage};
use super::inline::{body_of, failed, Saved, StepCtx};
use super::{notices, EngineDeps, ModelStop};

const DRAFT_TITLE: &str = "Draft";
const CHECK_TITLE: &str = "Check";

/// What a maker and its critic are both shown.
struct Asking {
    anchor: Anchor,
    sheet: String,
    answered: Vec<(String, String)>,
    timezone: String,
}

async fn asking(deps: &EngineDeps, job: &EngineJob) -> anyhow::Result<Asking> {
    let anchor_id = job
        .anchor_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("the job names no anchor"))?;
    let anchor = deps
        .anchors
        .find(anchor_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("its anchor is gone"))?;
    let member = anchor.profile_id.as_str();
    let sheet = sheet_for_brief(&deps.facts.list(member, FactState::Active).await?);
    let answered = deps
        .questions
        .answered(member, MAX_ANSWERS)
        .await?
        .into_iter()
        .filter_map(|asked| Some((asked.text, asked.answer?.as_words())))
        .collect();
    let (timezone, _) = deps.clock_and_place().await;
    Ok(Asking {
        anchor,
        sheet,
        answered,
        timezone,
    })
}

fn questions(count: usize) -> String {
    match count {
        1 => "one question".to_string(),
        count => format!("{count} questions"),
    }
}

/// The maker: at most four questions whose answers would change the work.
pub(super) async fn make(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let shown = asking(ctx.deps, ctx.job).await.map_err(failed)?;
    let message = build_questions_message(
        &shown.anchor,
        &shown.sheet,
        &shown.answered,
        &shown.timezone,
    );
    let reply = ctx.ask(QUESTIONS_SYSTEM, message).await?;
    let wrote = parse_questions(
        &reply,
        &shown.anchor.profile_id,
        &ctx.job.id,
        &shown.anchor.id,
        ctx.now,
    )
    .len();
    let summary = match wrote {
        0 => "Found nothing that needs asking.".to_string(),
        count => format!("Wrote {}.", questions(count)),
    };
    let saved = ctx
        .save(ArtifactKind::Data, DRAFT_TITLE, &summary, reply)
        .await?;
    Ok(Saved {
        summary,
        artifacts: vec![saved],
    })
}

/// The critic: drops a question the facts or stored answers already settle.
pub(super) async fn critic(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let shown = asking(ctx.deps, ctx.job).await.map_err(failed)?;
    let (_, draft) = ctx.draft().await?;
    let batch = parse_questions(
        &draft,
        &shown.anchor.profile_id,
        &ctx.job.id,
        &shown.anchor.id,
        ctx.now,
    );
    if batch.is_empty() {
        return Err(failed("the draft holds no question to check"));
    }
    let message = build_questions_critic_message(
        &shown.anchor,
        &shown.sheet,
        &shown.answered,
        &batch,
        &shown.timezone,
    );
    let reply = ctx.ask(QUESTIONS_CRITIC_SYSTEM, message).await?;
    let kept = batch.len() - parse_question_drops(&reply, batch.len()).len();
    let summary = format!("The check kept {kept} of {}.", questions(batch.len()));
    let saved = ctx
        .save(ArtifactKind::Data, CHECK_TITLE, &summary, reply)
        .await?;
    Ok(Saved {
        summary,
        artifacts: vec![saved],
    })
}

fn dropped(why: &str) -> (JobState, String) {
    (JobState::Dropped, format!("Dropped: {why}"))
}

/// What follows a questions task's steps: its one check, then the batch is stored and its
/// member told, or nothing is asked.
pub(super) async fn after_steps(
    deps: &EngineDeps,
    task: &LongTask,
    mut job: EngineJob,
    steps: Vec<PlanStep>,
    artifacts: Vec<ArtifactSummary>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let at = stage(&steps);
    let found = draft_artifact(&steps, &artifacts).filter(|_| !matches!(at, Stage::NoDraft));
    let (Some(found), Ok(shown)) = (found, asking(deps, &job).await) else {
        let outcome = why_no_draft(&steps);
        return close(deps, task, &mut job, JobState::Failed, &outcome, now).await;
    };
    let anchor = shown.anchor;
    let member = anchor.profile_id.as_str();
    let draft = body_of(deps, task, found).await?;
    let batch = parse_questions(&draft, member, &job.id, &anchor.id, now);
    let mut asked = Vec::new();
    let (state, said) = match at {
        Stage::Unchecked if batch.is_empty() => dropped("nothing needed asking."),
        Stage::Unchecked => match critic_step(task, &steps, &artifacts) {
            Ok(plan) => return deps.tasks.replace_open_steps(&plan).await,
            Err(_) => (JobState::Dropped, job_outcome_line(&job, None)),
        },
        Stage::NoDraft | Stage::CheckDied => (JobState::Dropped, job_outcome_line(&job, None)),
        Stage::Checked => {
            let reply = match verdict_artifact(&steps, &artifacts) {
                Some(found) => body_of(deps, task, found).await?,
                None => String::new(),
            };
            job.count_round(Vec::new());
            let drops = parse_question_drops(&reply, batch.len());
            asked = deps
                .questions
                .add_batch(&without_dropped(batch, &drops))
                .await?;
            if asked.is_empty() {
                dropped("the check left no question that was new and worth asking.")
            } else {
                deps.questions.mark_asked(member, &job.id, now).await?;
                let passed = Verdict {
                    pass: true,
                    objections: Vec::new(),
                };
                let said = format!(
                    "{} Asked {}.",
                    job_outcome_line(&job, Some(&passed)),
                    questions(asked.len())
                );
                (JobState::Passed, said)
            }
        }
    };
    close(deps, task, &mut job, state, &said, now).await?;
    notices::questions_asked(deps, &anchor, &asked, now).await;
    Ok(())
}
