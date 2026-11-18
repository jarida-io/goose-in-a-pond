//! A deliverable job: the brief its research task opens with, its critic and reviser steps,
//! and how it settles.

use chrono::{DateTime, Utc};

use pond_core::context::retrieval_service::Recollection;
use pond_core::shared::domain::long_task::{
    Artifact, ArtifactKind, ArtifactSummary, LongTask, PlanStep, StepState,
};
use pond_core::shared::services::long_task_roles::VERIFIER_ROLE;
use pond_core::shared::services::long_task_runner::{result_artifact, RESULT_TITLE};
use pond_core::shared::services::research_report as report;
use pond_core::user_data::domain::anchor::{
    refresh_due_after, Anchor, AnchorState, Deliverable, DeliverableState, SourceLink,
};
use pond_core::user_data::domain::dont_list::{DontRule, Strength};
use pond_core::user_data::domain::engine_job::{EngineJob, JobState};
use pond_core::user_data::domain::fact_sheet::FactState;
use pond_core::user_data::domain::question::QuestionState;
use pond_core::user_data::domain::specialist::specialist;
use pond_core::user_data::services::dont_list::{for_brief, AVOID_RULES_PER_BRIEF};
use pond_core::user_data::services::engine::brief::{deliverable_brief, BriefParts};
use pond_core::user_data::services::engine::critic::{
    build_critic_message, parse_verdict, CriticParts, SourceNote, Verdict, CRITIC_SYSTEM,
};
use pond_core::user_data::services::engine::is_inline_role;
use pond_core::user_data::services::engine::recipes::{goal_for, slot_named};
use pond_core::user_data::services::engine::revise::{
    build_revise_message, clean_revision, REVISE_SYSTEM,
};
use pond_core::user_data::services::engine::settle::{
    draft_from_result, job_outcome_line, split_changed,
};
use pond_core::user_data::services::engine::steps::{draft_artifact, latest_draft_step};
use pond_core::user_data::services::engine::tick::RETRY_AFTER;
use pond_core::user_data::services::fact_sheet::sheet_for_brief;

use super::finish::{close, critic_loop, ruling, Loop};
use super::inline::{body_of, failed, Saved, StepCtx};
use super::{notices, prompt_budget, window_chars, EngineDeps, ModelStop};

/// Things recalled for one brief, before they are sorted into rules, answers and captures.
const RECALL_HITS: usize = 120;
const OTHER_ANSWERS: usize = 5;
const CAPTURES_PER_BRIEF: usize = 5;
/// Source notes one critic or reviser is shown at most.
const MAX_NOTES: usize = 24;
/// The window's share a brief may fill: every step of the task reads it whole.
const BRIEF_SHARE: usize = 8;
const CHECK_TITLE: &str = "Check";

fn chars(text: &str) -> usize {
    text.chars().count()
}

/// The anchor and the deliverable a job serves.
pub(super) async fn subject(
    deps: &EngineDeps,
    job: &EngineJob,
) -> anyhow::Result<(Anchor, Deliverable)> {
    let anchor_id = job
        .anchor_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("the job names no anchor"))?;
    let deliverable_id = job
        .deliverable_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("the job names no deliverable"))?;
    let anchor = deps
        .anchors
        .find(anchor_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("its anchor is gone"))?;
    let deliverable = deps
        .anchors
        .find_deliverable(deliverable_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("its deliverable is gone"))?;
    Ok((anchor, deliverable))
}

async fn recalled(deps: &EngineDeps, member: &str, words: &str) -> Vec<Recollection> {
    match &deps.recall {
        Some(recall) => recall.recall(words, member, RECALL_HITS).await,
        None => Vec::new(),
    }
}

/// The ids of the recalled rows of one kind, most relevant first.
fn ids_of<'a>(hits: &'a [Recollection], prefix: &'a str) -> impl Iterator<Item = &'a str> {
    hits.iter()
        .filter_map(move |hit| hit.row_id.strip_prefix(prefix))
}

/// The don't-list as a brief or a critic reads it: every `never` rule, then the `avoid` rules
/// the recall found nearest, topped up with the newest.
async fn dont_list(
    deps: &EngineDeps,
    member: &str,
    hits: &[Recollection],
) -> anyhow::Result<String> {
    let never = deps.dont.list(member, Some(Strength::Never)).await?;
    let avoid = deps.dont.list(member, Some(Strength::Avoid)).await?;
    let mut relevant: Vec<DontRule> = ids_of(hits, "dont:")
        .filter_map(|id| avoid.iter().find(|rule| rule.id == id).cloned())
        .collect();
    for rule in &avoid {
        if relevant.len() >= AVOID_RULES_PER_BRIEF {
            break;
        }
        if !relevant.iter().any(|held| held.id == rule.id) {
            relevant.push(rule.clone());
        }
    }
    Ok(for_brief(&never, &relevant))
}

async fn sheet_of(deps: &EngineDeps, member: &str) -> anyhow::Result<String> {
    Ok(sheet_for_brief(
        &deps.facts.list(member, FactState::Active).await?,
    ))
}

/// The anchor's other deliverables a member can read, as title and hook.
async fn others_of(
    deps: &EngineDeps,
    deliverable: &Deliverable,
) -> anyhow::Result<Vec<(String, String)>> {
    Ok(deps
        .anchors
        .deliverables_of(&deliverable.anchor_id)
        .await?
        .into_iter()
        .filter(|other| other.id != deliverable.id && other.state.is_shown())
        .map(|other| (other.title, other.hook))
        .collect())
}

/// What a deliverable job's research task opens with.
pub(super) struct Opening {
    pub goal: String,
    pub brief: String,
    pub anchor: Anchor,
    pub deliverable: Deliverable,
}

/// The goal and the opening brief of the task that makes or refreshes `job`'s deliverable.
pub(super) async fn opening(
    deps: &EngineDeps,
    job: &EngineJob,
    now: DateTime<Utc>,
) -> anyhow::Result<Opening> {
    let (anchor, deliverable) = subject(deps, job).await?;
    anyhow::ensure!(
        anchor.state == AnchorState::Active,
        "its anchor is no longer active"
    );
    anyhow::ensure!(
        matches!(
            deliverable.state,
            DeliverableState::Queued | DeliverableState::Ready | DeliverableState::Unsure
        ),
        "its deliverable is {}",
        deliverable.state.as_str()
    );
    let slot = slot_named(&anchor, &deliverable.slot)
        .ok_or_else(|| anyhow::anyhow!("its anchor has no slot named {}", deliverable.slot))?;
    let member = anchor.profile_id.as_str();
    let (timezone, place) = deps.clock_and_place().await;
    let refresh = deliverable.version > 0;

    let playbook = match anchor.agent {
        Some(agent) if deps.agents.switch(member, agent).await?.is_on() => {
            Some(specialist(agent).playbook)
        }
        _ => None,
    };
    let hits = recalled(deps, member, &anchor.title).await;
    let sheet = sheet_of(deps, member).await?;
    let dont = dont_list(deps, member, &hits).await?;
    let answers: Vec<(String, String)> = deps
        .questions
        .for_anchor(member, &anchor.id)
        .await?
        .into_iter()
        .filter(|asked| asked.state == QuestionState::Answered)
        .filter_map(|asked| Some((asked.text, asked.answer?.as_words())))
        .collect();
    let mut other_answers = Vec::new();
    for id in ids_of(&hits, "answer:") {
        if other_answers.len() == OTHER_ANSWERS {
            break;
        }
        let Some(asked) = deps.questions.get(member, id).await? else {
            continue;
        };
        let elsewhere = asked.anchor_id.as_deref() != Some(anchor.id.as_str());
        if let (true, Some(answer)) = (elsewhere, &asked.answer) {
            other_answers.push((asked.text.clone(), answer.as_words()));
        }
    }
    let mut captures = Vec::new();
    for id in ids_of(&hits, "capture:") {
        if captures.len() == CAPTURES_PER_BRIEF {
            break;
        }
        let Some(capture) = deps.captures.get(member, id).await? else {
            continue;
        };
        if let (true, Some(shows)) = (capture.is_searchable(), &capture.description) {
            let details = capture.details.as_line();
            captures.push(if details.is_empty() {
                shows.clone()
            } else {
                format!("{shows}. {details}")
            });
        }
    }
    let others = others_of(deps, &deliverable).await?;

    let brief = deliverable_brief(
        &BriefParts {
            now,
            timezone: &timezone,
            member_place: place.as_deref(),
            anchor: &anchor,
            slot,
            playbook,
            fact_sheet: &sheet,
            dont_list: &dont,
            answers: &answers,
            other_answers: &other_answers,
            captures: &captures,
            other_deliverables: &others,
            last_version: refresh.then_some(deliverable.body.as_str()),
        },
        window_chars() / BRIEF_SHARE,
    );
    Ok(Opening {
        goal: goal_for(&anchor, slot, refresh, &timezone),
        brief,
        anchor,
        deliverable,
    })
}

/// The step that wrote the document: the last finished one that is neither a check nor a
/// correction, so every round of the loop is shown the same notes.
fn writing_step(steps: &[PlanStep]) -> Option<&PlanStep> {
    steps
        .iter()
        .filter(|step| {
            step.state() == StepState::Completed
                && !is_inline_role(step.role())
                && step.role() != VERIFIER_ROLE
        })
        .max_by_key(|step| step.ordinal())
}

/// What the research steps found and noted, oldest first, within `room` characters: never what
/// the writing step or a check saved, and never so much that the fact sheet is crowded out.
async fn source_notes(ctx: &StepCtx<'_>, room: usize) -> Result<Vec<(String, String)>, ModelStop> {
    let writer = writing_step(ctx.steps).map(|step| step.key());
    let by_research = |artifact: &&ArtifactSummary| {
        let Some(key) = artifact.step_key.as_ref() else {
            return false;
        };
        let inline = ctx
            .steps
            .iter()
            .any(|step| step.key() == key && is_inline_role(step.role()));
        matches!(artifact.kind, ArtifactKind::Findings | ArtifactKind::Note)
            && Some(key) != writer
            && !inline
    };
    let mut notes = Vec::new();
    let mut left = room;
    for artifact in ctx.artifacts.iter().filter(by_research).take(MAX_NOTES) {
        if left == 0 {
            break;
        }
        let body = ctx.body_of(artifact).await?;
        let body: String = body.trim().chars().take(left).collect();
        left = left.saturating_sub(chars(&body) + chars(&artifact.title));
        notes.push((artifact.title.clone(), body));
    }
    Ok(notes)
}

fn as_notes(notes: &[(String, String)]) -> Vec<SourceNote<'_>> {
    notes
        .iter()
        .map(|(title, body)| SourceNote { title, body })
        .collect()
}

/// The check of the draft as it stands; its raw reply is the verdict the loop reads.
pub(super) async fn critic(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let (anchor, deliverable) = subject(ctx.deps, ctx.job).await.map_err(failed)?;
    let member = anchor.profile_id.as_str();
    let (_, draft) = ctx.draft().await?;
    let brief = ctx.brief().await?;
    let hits = recalled(ctx.deps, member, &anchor.title).await;
    let sheet = sheet_of(ctx.deps, member).await.map_err(failed)?;
    let dont = dont_list(ctx.deps, member, &hits).await.map_err(failed)?;
    let titles: Vec<String> = others_of(ctx.deps, &deliverable)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(title, _)| title)
        .collect();

    let budget = prompt_budget(CRITIC_SYSTEM);
    let spoken_for =
        chars(&draft) + chars(&dont) + chars(&brief) + chars(&sheet) + titles.join("\n").len();
    let notes = source_notes(ctx, budget.saturating_sub(spoken_for)).await?;
    let message = build_critic_message(
        &CriticParts {
            brief: &brief,
            draft: &draft,
            notes: &as_notes(&notes),
            fact_sheet: &sheet,
            dont_list: &dont,
            existing_titles: &titles,
        },
        budget,
    );
    let reply = ctx.ask(CRITIC_SYSTEM, message).await?;
    let verdict = parse_verdict(&reply);
    let summary = match (verdict.pass, verdict.objections.len()) {
        (true, _) => "The check passed.".to_string(),
        _ if verdict.is_unread() => "The check could not be read.".to_string(),
        (false, 1) => "The check held one point against the draft.".to_string(),
        (false, held) => format!("The check held {held} points against the draft."),
    };
    let saved = ctx
        .save(ArtifactKind::Data, CHECK_TITLE, &summary, reply)
        .await?;
    Ok(Saved {
        summary,
        artifacts: vec![saved],
    })
}

/// The correction of a failed draft: a usable one takes the place of the task's Result.
pub(super) async fn revise(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let (held, draft) = ctx.draft().await?;
    let verdict = parse_verdict(&ctx.verdict().await?);
    let brief = ctx.brief().await?;
    let window = window_chars();
    // The reply is the whole draft again, so the draft is counted twice.
    let reply_room = (window / 4).max(chars(&draft) + chars(&draft) / 10);
    let budget = window.saturating_sub(chars(REVISE_SYSTEM) + reply_room);
    if budget < chars(&draft) {
        return Err(failed("the draft is too long to correct in one pass"));
    }
    let notes = source_notes(ctx, budget.saturating_sub(chars(&draft) + chars(&brief))).await?;
    let message = build_revise_message(
        &brief,
        &draft,
        &verdict.objections,
        &as_notes(&notes),
        budget,
    );
    let reply = ctx.ask(REVISE_SYSTEM, message).await?;
    let revised = clean_revision(&reply, &draft)
        .ok_or_else(|| failed("the correction came back too short to use"))?;
    let saved = ctx
        .save(ArtifactKind::Draft, RESULT_TITLE, &held.summary, revised)
        .await?;
    if let Err(e) = ctx
        .deps
        .tasks
        .delete_artifact(ctx.task.id(), &held.id)
        .await
    {
        tracing::warn!(task = %ctx.task.id(), error = %e, "a corrected draft's old version stayed");
    }
    Ok(Saved {
        summary: "Corrected the draft where the check objected.".to_string(),
        artifacts: vec![saved],
    })
}

/// Saves the whole report as the task's Result before its first check: a research task's
/// answer is its summary and the sections its steps drafted, and the critic must read all of it.
async fn join_report(
    deps: &EngineDeps,
    task: &LongTask,
    deliverable: &Deliverable,
    steps: &[PlanStep],
    artifacts: &mut Vec<ArtifactSummary>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let Some(writer) = latest_draft_step(steps) else {
        return Ok(());
    };
    let parts = report::report_parts(steps, artifacts);
    let find = |id: &String| artifacts.iter().find(|artifact| &artifact.id == id);
    let (summary, said) = match parts.result.as_ref().and_then(find) {
        Some(found) => (
            Some(body_of(deps, task, found).await?),
            found.summary.clone(),
        ),
        None => (None, deliverable.title.clone()),
    };
    let mut sections = Vec::new();
    for found in parts.sections.iter().filter_map(find) {
        sections.push((found.title.clone(), body_of(deps, task, found).await?));
    }
    if summary.is_none() && sections.is_empty() {
        return Ok(());
    }
    let whole = report::assemble(&deliverable.title, summary.as_deref(), &sections);
    let joined = Artifact::new(
        task.id(),
        Some(writer.key().clone()),
        ArtifactKind::Draft,
        RESULT_TITLE,
        &said,
        whole,
        now,
    )?;
    deps.tasks.insert_artifact(&joined).await?;
    artifacts.push(joined.summary_view());
    Ok(())
}

/// A deliverable whose job ended with nothing to show goes back to what the member last had,
/// and waits before it is tried again.
pub(super) async fn rest(deps: &EngineDeps, job: &EngineJob, now: DateTime<Utc>) {
    if let Some(id) = job.deliverable_id.as_deref() {
        release(deps, id, now).await;
    }
}

/// Puts one deliverable back to what the member last had, with the wait before its next try.
pub(super) async fn release(deps: &EngineDeps, deliverable_id: &str, now: DateTime<Utc>) {
    let Ok(Some(mut deliverable)) = deps.anchors.find_deliverable(deliverable_id).await else {
        return;
    };
    if deliverable.state == DeliverableState::Making {
        let _ = deliverable.release(now);
    }
    match deliverable.state {
        DeliverableState::Queued => deliverable.updated_at = now,
        DeliverableState::Ready | DeliverableState::Unsure => {
            let retry = now + RETRY_AFTER;
            if deliverable.refresh_at.is_some_and(|due| due < retry) {
                deliverable.refresh_at = Some(retry);
            }
        }
        _ => return,
    }
    if let Err(e) = deps.anchors.save_deliverable(&deliverable).await {
        tracing::warn!(deliverable = %deliverable.id, error = %e, "a deliverable could not be released");
    }
}

#[allow(clippy::too_many_arguments)]
async fn settle(
    deps: &EngineDeps,
    task: &LongTask,
    job: &mut EngineJob,
    state: JobState,
    verdict: Option<&Verdict>,
    steps: &[PlanStep],
    artifacts: &[ArtifactSummary],
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let shown = matches!(state, JobState::Passed | JobState::Unsure);
    let body = match draft_artifact(steps, artifacts).filter(|_| shown) {
        Some(found) => Some(body_of(deps, task, found).await?),
        None => None,
    };
    let (Some(body), Ok((anchor, mut deliverable))) = (body, subject(deps, job).await) else {
        rest(deps, job, now).await;
        return close(
            deps,
            task,
            job,
            JobState::Failed,
            "Failed: its steps saved nothing to show.",
            now,
        )
        .await;
    };
    let (changed, body) = if deliverable.version > 0 {
        split_changed(&body)
    } else {
        (String::new(), body)
    };
    let cited = report::sources(&body)
        .into_iter()
        .map(|source| SourceLink {
            title: source.title,
            url: source.url,
        })
        .collect();
    let draft = draft_from_result(&body, cited);
    let refresh_at = Some(now + refresh_due_after(anchor.starts_at, now));
    let moved = if state == JobState::Passed {
        deliverable.settle_ready(draft, &changed, refresh_at, now)
    } else {
        let held = verdict.map(|v| v.objections.clone()).unwrap_or_default();
        deliverable
            .settle_unsure(draft, held, refresh_at, now)
            .map(|()| true)
    };
    let Ok(tell) = moved else {
        return close(
            deps,
            task,
            job,
            JobState::Cancelled,
            "Stopped: what it was making is no longer wanted.",
            now,
        )
        .await;
    };
    deps.anchors.save_deliverable(&deliverable).await?;
    let outcome = job_outcome_line(job, verdict);
    close(deps, task, job, state, &outcome, now).await?;
    if tell {
        notices::deliverable_ready(deps, &anchor, &deliverable, now).await;
    }
    Ok(())
}

/// What follows a deliverable task's steps: a check, a correction, or the settle.
pub(super) async fn after_steps(
    deps: &EngineDeps,
    task: &LongTask,
    mut job: EngineJob,
    steps: Vec<PlanStep>,
    mut artifacts: Vec<ArtifactSummary>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    if !steps.iter().any(|step| is_inline_role(step.role())) {
        if let Ok((_, deliverable)) = subject(deps, &job).await {
            join_report(deps, task, &deliverable, &steps, &mut artifacts, now).await?;
        }
    }
    match critic_loop(deps, task, &mut job, &steps, &artifacts, now).await? {
        Loop::Added => Ok(()),
        Loop::Settle(state, verdict) => {
            settle(
                deps,
                task,
                &mut job,
                state,
                verdict.as_ref(),
                &steps,
                &artifacts,
                now,
            )
            .await
        }
    }
}

/// A deliverable task the runner gave up on: shown unsure when its document was written,
/// failed when it was not.
pub(super) async fn out_of_road(
    deps: &EngineDeps,
    task: &LongTask,
    mut job: EngineJob,
    reason: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let steps = deps.tasks.list_steps(task.id()).await?;
    let mut artifacts = deps.tasks.list_artifacts(task.id()).await?;
    if result_artifact(&artifacts).is_none() || latest_draft_step(&steps).is_none() {
        rest(deps, &job, now).await;
        return close(
            deps,
            task,
            &mut job,
            JobState::Failed,
            &format!("Failed: {reason}."),
            now,
        )
        .await;
    }
    if !steps.iter().any(|step| is_inline_role(step.role())) {
        if let Ok((_, deliverable)) = subject(deps, &job).await {
            join_report(deps, task, &deliverable, &steps, &mut artifacts, now).await?;
        }
    }
    let verdict = ruling(deps, task, &steps, &artifacts).await?;
    settle(
        deps,
        task,
        &mut job,
        JobState::Unsure,
        verdict.as_ref(),
        &steps,
        &artifacts,
        now,
    )
    .await
}
