//! A fact-sheet job: the maker that writes the sheet again from numbered sources, the critic
//! that names lines to drop, and the replace that follows.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use pond_core::shared::domain::long_task::{ArtifactKind, ArtifactSummary, LongTask, PlanStep};
use pond_core::shared::domain::todo::{zone, Tz};
use pond_core::shared::services::goose_thread::is_thread;
use pond_core::user_data::domain::anchor::AnchorState;
use pond_core::user_data::domain::dont_list::Strength;
use pond_core::user_data::domain::engine_job::{EngineJob, JobState};
use pond_core::user_data::domain::fact_sheet::{FactSourceKind, FactState};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::services::engine::critic::Verdict;
use pond_core::user_data::services::engine::settle::job_outcome_line;
use pond_core::user_data::services::engine::steps::{
    critic_step, draft_artifact, latest_draft_step, verdict_artifact,
};
use pond_core::user_data::services::fact_sheet::{
    build_critic_message, build_message, counting_use, judged_drops, merged_draft, parse, passes,
    sources, surviving, ConversationSummary, Drafted, Outcome, Source, SourceRows, ToolHint,
    CRITIC_SYSTEM, MAX_ANSWERS, MAX_SUMMARIES, SYSTEM,
};

use super::finish::{close, stage, why_no_draft, Stage};
use super::inline::{body_of, failed, Saved, StepCtx};
use super::{EngineDeps, ModelStop};

/// Memories read for one rebuild, before the most important are kept.
const MEMORY_ROWS: usize = 400;
/// Conversations looked through for ones with a summary.
const SESSIONS_SCANNED: usize = 80;
const SOURCES_TITLE: &str = "Sources";
const DRAFT_TITLE: &str = "Draft";
const CHECK_TITLE: &str = "Check";

/// What the maker was shown, kept so the check and the settle read the draft against the same
/// numbers.
#[derive(Serialize, Deserialize)]
struct Shown {
    sources: Vec<ShownSource>,
    tools: Vec<ShownTool>,
}

#[derive(Serialize, Deserialize)]
struct ShownSource {
    kind: String,
    reference: String,
    text: String,
    at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize)]
struct ShownTool {
    name: String,
    about: String,
}

impl Shown {
    fn of(sources: &[Source], tools: &[ToolHint]) -> Self {
        Self {
            sources: sources
                .iter()
                .map(|source| ShownSource {
                    kind: source.kind.as_str().to_string(),
                    reference: source.reference.clone(),
                    text: source.text.clone(),
                    at: source.at,
                })
                .collect(),
            tools: tools
                .iter()
                .map(|tool| ShownTool {
                    name: tool.name.clone(),
                    about: tool.about.clone(),
                })
                .collect(),
        }
    }

    fn read(self) -> (Vec<Source>, Vec<ToolHint>) {
        let sources = self
            .sources
            .into_iter()
            .filter_map(|source| {
                Some(Source {
                    kind: FactSourceKind::parse(&source.kind)?,
                    reference: source.reference,
                    text: source.text,
                    at: source.at,
                })
            })
            .collect();
        let tools = self
            .tools
            .into_iter()
            .map(|tool| ToolHint {
                name: tool.name,
                about: tool.about,
            })
            .collect();
        (sources, tools)
    }
}

/// The member's conversations that have a rolling summary, newest first.
async fn summaries_of(deps: &EngineDeps, member: &str) -> Vec<ConversationSummary> {
    let sessions = deps.sessions.list_sessions().await.unwrap_or_default();
    let mut found = Vec::new();
    for session in sessions
        .into_iter()
        .filter(|session| session.profile_id.as_deref() == Some(member) && !is_thread(&session.id))
        .take(SESSIONS_SCANNED)
    {
        if found.len() == MAX_SUMMARIES {
            break;
        }
        let Ok((Some(summary), _)) = deps.sessions.get_rolling_summary(&session.id).await else {
            continue;
        };
        if !summary.trim().is_empty() {
            found.push(ConversationSummary {
                session_id: session.id,
                title: session.title.unwrap_or_default(),
                summary,
                updated_at: session.updated_at,
            });
        }
    }
    found
}

/// The numbered sources one rebuild rests on.
async fn sources_for(
    deps: &EngineDeps,
    member: &str,
    tz: Tz,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Source>> {
    let memories = deps
        .memories
        .search_recent(&ProfileScope::Owner(member.to_string()), MEMORY_ROWS)
        .await?;
    let summaries = summaries_of(deps, member).await;
    let interests = deps.interests.list(member).await?;
    let anchors = deps.anchors.list(member, &[AnchorState::Active]).await?;
    let answers = deps.questions.answered(member, MAX_ANSWERS).await?;
    Ok(sources(
        member,
        SourceRows {
            memories,
            summaries: &summaries,
            interests: &interests,
            anchors: &anchors,
            answers: &answers,
        },
        tz,
        now,
    ))
}

/// The sources and tools the task's maker step saved.
async fn shown(
    deps: &EngineDeps,
    task: &LongTask,
    steps: &[PlanStep],
    artifacts: &[ArtifactSummary],
) -> anyhow::Result<(Vec<Source>, Vec<ToolHint>)> {
    let maker = latest_draft_step(steps).map(|step| step.key());
    let saved = artifacts
        .iter()
        .rev()
        .find(|artifact| artifact.step_key.as_ref() == maker && artifact.title == SOURCES_TITLE)
        .ok_or_else(|| anyhow::anyhow!("the draft's sources were not saved"))?;
    let shown: Shown = serde_json::from_str(&body_of(deps, task, saved).await?)?;
    Ok(shown.read())
}

/// The maker: the whole sheet written again from the member's sources.
pub(super) async fn make(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let member = ctx.member()?;
    let (timezone, _) = ctx.deps.clock_and_place().await;
    let sources = sources_for(ctx.deps, member, zone(&timezone), ctx.now)
        .await
        .map_err(failed)?;
    let mut sheet = ctx
        .deps
        .facts
        .list(member, FactState::Active)
        .await
        .map_err(failed)?;
    sheet.extend(
        ctx.deps
            .facts
            .list(member, FactState::Removed)
            .await
            .map_err(failed)?,
    );
    let tools = (ctx.deps.tools)();
    let never = ctx
        .deps
        .dont
        .list(member, Some(Strength::Never))
        .await
        .map_err(failed)?;
    let mut parts: Vec<(usize, Outcome)> = Vec::new();
    for slice in passes(sources.len()) {
        let shown = &sources[slice.clone()];
        let reply = ctx
            .ask(SYSTEM, build_message(shown, &sheet, &tools, &never))
            .await?;
        parts.push((slice.start, parse(&reply, shown, &tools, member, ctx.now)));
    }
    let reply = merged_draft(&parts);

    let shown = serde_json::to_string(&Shown::of(&sources, &tools)).map_err(failed)?;
    let kept = ctx
        .save(
            ArtifactKind::Data,
            SOURCES_TITLE,
            "The numbered sources and the tools the draft was written from",
            shown,
        )
        .await?;
    // Saved last: the draft a step stands by is what it saved last.
    let draft = ctx
        .save(
            ArtifactKind::Data,
            DRAFT_TITLE,
            "The fact sheet as drafted",
            reply,
        )
        .await?;
    Ok(Saved {
        summary: match sources.len() {
            1 => "Drafted the fact sheet from one source.".to_string(),
            count => format!("Drafted the fact sheet from {count} sources."),
        },
        artifacts: vec![kept, draft],
    })
}

/// The critic: names the drafted lines to drop.
pub(super) async fn critic(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let member = ctx.member()?;
    let (sources, tools) = shown(ctx.deps, ctx.task, ctx.steps, ctx.artifacts)
        .await
        .map_err(failed)?;
    let (_, draft) = ctx.draft().await?;
    let drafted = parse(&draft, &sources, &tools, member, ctx.now).drafted;
    if drafted.is_empty() {
        return Err(failed("the draft holds no line to check"));
    }
    let reply = ctx
        .ask(CRITIC_SYSTEM, build_critic_message(&drafted, &sources))
        .await?;
    let summary = match judged_drops(&reply, &drafted, &sources) {
        None => "The check could not be read.".to_string(),
        Some(drops) if drops.is_empty() => "The check kept every line.".to_string(),
        Some(drops) => format!(
            "The check dropped {} of {} lines.",
            drops.len(),
            drafted.len()
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

fn lines(count: usize) -> String {
    match count {
        1 => "one line".to_string(),
        count => format!("{count} lines"),
    }
}

fn dropped(why: &str) -> (JobState, String) {
    (JobState::Dropped, format!("Dropped: {why}"))
}

/// Reads the check's reply and writes the lines that survived it over the member's sheet.
#[allow(clippy::too_many_arguments)]
async fn replace(
    deps: &EngineDeps,
    task: &LongTask,
    job: &mut EngineJob,
    member: &str,
    drafted: Vec<Drafted>,
    sources: &[Source],
    steps: &[PlanStep],
    artifacts: &[ArtifactSummary],
    now: DateTime<Utc>,
) -> anyhow::Result<(JobState, String)> {
    let reply = match verdict_artifact(steps, artifacts) {
        Some(found) => body_of(deps, task, found).await?,
        None => String::new(),
    };
    job.count_round(Vec::new());
    let Some(drops) = judged_drops(&reply, &drafted, sources) else {
        let said = job_outcome_line(job, Some(&Verdict::unread()));
        return Ok((JobState::Dropped, said));
    };
    let kept = surviving(drafted, &drops);
    if kept.is_empty() {
        return Ok(dropped("the check kept no line."));
    }
    let sheet = deps.facts.list(member, FactState::Active).await?;
    let active = deps
        .facts
        .replace(member, &counting_use(kept, &sheet), now)
        .await?;
    let passed = Verdict {
        pass: true,
        objections: Vec::new(),
    };
    let said = format!(
        "{} The sheet now holds {}; the check dropped {}.",
        job_outcome_line(job, Some(&passed)),
        lines(active.len()),
        lines(drops.len())
    );
    Ok((JobState::Passed, said))
}

/// What follows a fact-sheet task's steps: its one check, then the replace or the drop.
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
    let (Some(found), Some(member)) = (found, job.profile_id.clone()) else {
        let outcome = why_no_draft(&steps);
        return close(deps, task, &mut job, JobState::Failed, &outcome, now).await;
    };
    let draft = body_of(deps, task, found).await?;
    let Ok((sources, tools)) = shown(deps, task, &steps, &artifacts).await else {
        let outcome = "Failed: what the draft was written from was not kept.";
        return close(deps, task, &mut job, JobState::Failed, outcome, now).await;
    };
    let outcome = parse(&draft, &sources, &tools, &member, now);
    let (state, said) = match at {
        Stage::Unchecked if outcome.unparseable => dropped("the draft could not be read."),
        Stage::Unchecked if outcome.drafted.is_empty() => {
            dropped("the draft held no line its sources support.")
        }
        Stage::Unchecked => match critic_step(task, &steps, &artifacts) {
            Ok(plan) => return deps.tasks.replace_open_steps(&plan).await,
            Err(_) => (JobState::Dropped, job_outcome_line(&job, None)),
        },
        Stage::NoDraft | Stage::CheckDied => (JobState::Dropped, job_outcome_line(&job, None)),
        Stage::Checked => {
            replace(
                deps,
                task,
                &mut job,
                &member,
                outcome.drafted,
                &sources,
                &steps,
                &artifacts,
                now,
            )
            .await?
        }
    };
    close(deps, task, &mut job, state, &said, now).await
}
