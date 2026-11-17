//! A questions job: code picks the plan's open gaps, the maker words one question for each, the
//! critic drops what is already settled, and the batch that passes code's checks is stored.

use chrono::{DateTime, Utc};

use pond_core::shared::domain::long_task::{ArtifactKind, ArtifactSummary, LongTask, PlanStep};
use pond_core::user_data::domain::anchor::Anchor;
use pond_core::user_data::domain::engine_job::{EngineJob, JobState};
use pond_core::user_data::domain::fact_sheet::{FactLine, FactState};
use pond_core::user_data::domain::question::Question;
use pond_core::user_data::services::engine::critic::Verdict;
use pond_core::user_data::services::engine::gaps::{lines_for, next_round_number, open_gaps, Gap};
use pond_core::user_data::services::engine::questions::{
    build_questions_critic_message, build_questions_message, parse_question_drops, parse_questions,
    without_dropped, Shown, QUESTIONS_CRITIC_SYSTEM, QUESTIONS_SYSTEM,
};
use pond_core::user_data::services::engine::settle::job_outcome_line;
use pond_core::user_data::services::engine::steps::{
    critic_step, draft_artifact, verdict_artifact,
};
use pond_core::user_data::services::fact_sheet::{sheet_for_brief, MAX_ANSWERS};
use pond_core::user_data::services::question_desk::keep_new;

use super::finish::{close, stage, why_no_draft, Stage};
use super::inline::{body_of, failed, Saved, StepCtx};
use super::{EngineDeps, ModelStop};

const DRAFT_TITLE: &str = "Draft";
const CHECK_TITLE: &str = "Check";

/// The most of what the plan's work found that a maker is shown.
const SOURCE_CHARS: usize = 1_500;

/// What a maker and its critic are both shown.
struct Asking {
    anchor: Anchor,
    sheet: String,
    answered: Vec<(String, String)>,
    sources: String,
    gaps: Vec<&'static Gap>,
    /// Every question asked for the plan, in any state.
    asked: Vec<Question>,
    timezone: String,
}

impl Asking {
    fn shown(&self) -> Shown<'_> {
        Shown {
            anchor: &self.anchor,
            sheet: &self.sheet,
            answered: &self.answered,
            sources: &self.sources,
            gaps: &self.gaps,
            timezone: &self.timezone,
        }
    }
}

/// What the plan's deliverables already found, one line each, as the maker's options' sources.
async fn sources_of(deps: &EngineDeps, anchor: &Anchor) -> anyhow::Result<String> {
    let mut lines: Vec<String> = Vec::new();
    for made in deps.anchors.deliverables_of(&anchor.id).await? {
        if !made.state.is_shown() {
            continue;
        }
        if !made.hook.trim().is_empty() {
            lines.push(format!("- {}: {}", made.title, made.hook.trim()));
        }
        lines.extend(
            made.facts
                .iter()
                .filter(|fact| !fact.value.trim().is_empty())
                .map(|fact| format!("- {}: {}", fact.label, fact.value)),
        );
    }
    let mut out = String::new();
    for line in lines {
        if out.chars().count() + line.chars().count() > SOURCE_CHARS {
            break;
        }
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
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
    let asked = deps.questions.for_anchor(member, &anchor.id).await?;
    let gaps = open_gaps(&anchor, &asked);
    let lines: Vec<FactLine> = deps.facts.list(member, FactState::Active).await?;
    let relevant: Vec<FactLine> = lines_for(&lines, &gaps).into_iter().cloned().collect();
    let sheet = sheet_for_brief(&relevant);
    let answered = deps
        .questions
        .answered(member, MAX_ANSWERS)
        .await?
        .into_iter()
        .filter_map(|asked| Some((asked.text, asked.answer?.as_words())))
        .collect();
    let sources = sources_of(deps, &anchor).await?;
    let (timezone, _) = deps.clock_and_place().await;
    Ok(Asking {
        anchor,
        sheet,
        answered,
        sources,
        gaps,
        asked,
        timezone,
    })
}

fn questions(count: usize) -> String {
    match count {
        1 => "one question".to_string(),
        count => format!("{count} questions"),
    }
}

/// An empty batch, saved as the maker's draft when code finds no gap to ask about.
const NOTHING_TO_ASK: &str = "{\"questions\": []}";

/// The maker: one question for each open gap, at most four. With no gap open it asks the model
/// nothing.
pub(super) async fn make(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let shown = asking(ctx.deps, ctx.job).await.map_err(failed)?;
    if shown.gaps.is_empty() {
        let summary = "No detail is missing.".to_string();
        let saved = ctx
            .save(
                ArtifactKind::Data,
                DRAFT_TITLE,
                &summary,
                NOTHING_TO_ASK.into(),
            )
            .await?;
        return Ok(Saved {
            summary,
            artifacts: vec![saved],
        });
    }
    let message = build_questions_message(&shown.shown());
    let reply = ctx.ask(QUESTIONS_SYSTEM, message).await?;
    let wrote = parse_questions(
        &reply,
        &shown.anchor.profile_id,
        &ctx.job.id,
        &shown.anchor.id,
        &shown.gaps,
        ctx.now,
    )
    .len();
    let summary = match wrote {
        0 => "Wrote no question that fits.".to_string(),
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
        &shown.gaps,
        ctx.now,
    );
    if batch.is_empty() {
        return Err(failed("the draft holds no question to check"));
    }
    let message = build_questions_critic_message(&shown.shown(), &batch);
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

/// What follows a questions task's steps: its one check, then the batch that passes code's checks
/// is stored and its member told, or nothing is asked.
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
    let round = next_round_number(&shown.asked);
    let gaps = shown.gaps;
    let anchor = shown.anchor;
    let member = anchor.profile_id.as_str();
    let draft = body_of(deps, task, found).await?;
    let batch = parse_questions(&draft, member, &job.id, &anchor.id, &gaps, now);
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
            let mut kept = without_dropped(batch, &drops);
            for question in kept.iter_mut() {
                question.round = round;
            }
            let kept = keep_new(
                kept,
                member,
                deps.questions.as_ref(),
                deps.similar.as_deref(),
            )
            .await?;
            let asked = deps.questions.add_batch(&kept).await?;
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
    // Not pushed: questions wait in My Goose and on Home until the member looks.
    close(deps, task, &mut job, state, &said, now).await
}
