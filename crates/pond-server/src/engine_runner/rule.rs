//! A rule job: the maker that turns a member's "never this" and their words into one general
//! rule, the general critic loop, and on a pass the rule written over their own words, which it
//! keeps.

use chrono::{DateTime, Utc};

use pond_core::shared::domain::long_task::{ArtifactKind, ArtifactSummary, LongTask, PlanStep};
use pond_core::user_data::domain::dont_list::{DontRule, Strength};
use pond_core::user_data::domain::engine_job::{EngineJob, JobState};
use pond_core::user_data::domain::fact_sheet::FactState;
use pond_core::user_data::services::dont_list::{
    build_rule_message, for_brief, parse_rule, RULE_SYSTEM,
};
use pond_core::user_data::services::engine::critic::{
    build_critic_message, parse_verdict, CriticParts, CRITIC_SYSTEM,
};
use pond_core::user_data::services::engine::revise::{
    build_revise_message, clean_revision, REVISE_SYSTEM,
};
use pond_core::user_data::services::engine::settle::job_outcome_line;
use pond_core::user_data::services::engine::steps::draft_artifact;
use pond_core::user_data::services::fact_sheet::sheet_for_brief;

use super::finish::{close, critic_loop, stage, why_no_draft, Loop, Stage};
use super::inline::{body_of, failed, Saved, StepCtx};
use super::{prompt_budget, EngineDeps, ModelStop};

const DRAFT_TITLE: &str = "Draft";
const CHECK_TITLE: &str = "Check";
const BRIEF: &str = "Write one general rule for this member's don't-list from what they turned \
     down, so Goose never makes that kind of thing for them again.";
const WORDS_STAY: &str = "Their own words stay as the rule.";
const REPLY_CUE: &str = "JSON:";

/// The rule a job rewrites, the title of what was turned down and the maker's message.
struct TurnedDown {
    rule: DontRule,
    title: String,
    message: String,
}

impl TurnedDown {
    /// What the critic and the reviser are told the rule is for.
    fn brief(&self) -> String {
        let facts = self.message.trim_end();
        let facts = facts.strip_suffix(REPLY_CUE).unwrap_or(facts).trim_end();
        format!("{BRIEF}\n\n{facts}")
    }
}

async fn turned_down(deps: &EngineDeps, job: &EngineJob) -> anyhow::Result<TurnedDown> {
    let member = job
        .profile_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("the job names no member"))?;
    let rule = deps
        .dont
        .get(member, &job.subject)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the rule is gone"))?;
    let made = match rule.origin_ref.as_deref() {
        Some(id) => deps.anchors.find_deliverable(id).await?,
        None => None,
    }
    .ok_or_else(|| anyhow::anyhow!("what was turned down is gone"))?;
    let kind = format!("a {}", made.kind.as_str());
    let message = build_rule_message(
        &made.title,
        &kind,
        &made.hook,
        &rule.reasons,
        rule.said.as_deref(),
    );
    Ok(TurnedDown {
        rule,
        title: made.title,
        message,
    })
}

/// The maker: one general rule from the member's words and reasons.
pub(super) async fn make(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let shown = turned_down(ctx.deps, ctx.job).await.map_err(failed)?;
    let reply = ctx.ask(RULE_SYSTEM, shown.message.clone()).await?;
    let summary = match parse_rule(&reply, &shown.title) {
        Some(_) => "Wrote a general rule.",
        None => "Wrote no rule that could be used.",
    };
    let saved = ctx
        .save(ArtifactKind::Data, DRAFT_TITLE, summary, reply)
        .await?;
    Ok(Saved {
        summary: summary.to_string(),
        artifacts: vec![saved],
    })
}

/// The member's rules of one strength but `except`: shown, the rule being rewritten would make
/// the draft look a duplicate.
async fn other_rules(
    deps: &EngineDeps,
    except: &DontRule,
    strength: Strength,
) -> anyhow::Result<Vec<DontRule>> {
    let rules = deps.dont.list(&except.profile_id, Some(strength)).await?;
    Ok(rules
        .into_iter()
        .filter(|rule| rule.id != except.id)
        .collect())
}

/// The general critic, shown the member's sheet and their other rules.
pub(super) async fn critic(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let shown = turned_down(ctx.deps, ctx.job).await.map_err(failed)?;
    let member = shown.rule.profile_id.as_str();
    let (_, draft) = ctx.draft().await?;
    let sheet = sheet_for_brief(
        &ctx.deps
            .facts
            .list(member, FactState::Active)
            .await
            .map_err(failed)?,
    );
    let never = other_rules(ctx.deps, &shown.rule, Strength::Never)
        .await
        .map_err(failed)?;
    let avoid = other_rules(ctx.deps, &shown.rule, Strength::Avoid)
        .await
        .map_err(failed)?;
    let message = build_critic_message(
        &CriticParts {
            brief: &shown.brief(),
            draft: &draft,
            notes: &[],
            fact_sheet: &sheet,
            dont_list: &for_brief(&never, &avoid),
            existing_titles: &[],
        },
        prompt_budget(CRITIC_SYSTEM),
    );
    let reply = ctx.ask(CRITIC_SYSTEM, message).await?;
    let verdict = parse_verdict(&reply);
    let summary = match (verdict.pass, verdict.objections.len()) {
        (true, _) => "The check passed.".to_string(),
        _ if verdict.is_unread() => "The check could not be read.".to_string(),
        (false, 1) => "The check held one point against the rule.".to_string(),
        (false, held) => format!("The check held {held} points against the rule."),
    };
    let saved = ctx
        .save(ArtifactKind::Data, CHECK_TITLE, &summary, reply)
        .await?;
    Ok(Saved {
        summary,
        artifacts: vec![saved],
    })
}

/// The correction of a rule the check failed.
pub(super) async fn revise(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let shown = turned_down(ctx.deps, ctx.job).await.map_err(failed)?;
    let (_, draft) = ctx.draft().await?;
    let verdict = parse_verdict(&ctx.verdict().await?);
    let message = build_revise_message(
        &shown.brief(),
        &draft,
        &verdict.objections,
        &[],
        prompt_budget(REVISE_SYSTEM),
    );
    let reply = ctx.ask(REVISE_SYSTEM, message).await?;
    let revised = clean_revision(&reply, &draft)
        .ok_or_else(|| failed("the correction came back too short to use"))?;
    let saved = ctx
        .save(
            ArtifactKind::Data,
            DRAFT_TITLE,
            "The rule as corrected",
            revised,
        )
        .await?;
    Ok(Saved {
        summary: "Corrected the rule where the check objected.".to_string(),
        artifacts: vec![saved],
    })
}

/// Writes `text` as the rule in place of the member's words, which it keeps as what they said.
async fn rewrite(
    deps: &EngineDeps,
    held: &DontRule,
    text: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let Some(mut general) =
        DontRule::new(&held.profile_id, text, Strength::Never, held.origin, now)
    else {
        return Ok(false);
    };
    general.reasons = held.reasons.clone();
    general.said = held.said.clone().or_else(|| Some(held.rule.clone()));
    general.origin_ref = held.origin_ref.clone();
    general.created_at = held.created_at;
    let stored = deps.dont.add(&general).await?;
    // A new wording is a new key and so a new row; the old one goes once the new one is kept.
    if stored.id != held.id {
        deps.dont.delete(&held.profile_id, &held.id).await?;
    }
    Ok(true)
}

/// What follows a rule task's steps: the critic loop, then the rule rewritten on a pass, or
/// left as the member wrote it.
pub(super) async fn after_steps(
    deps: &EngineDeps,
    task: &LongTask,
    mut job: EngineJob,
    steps: Vec<PlanStep>,
    artifacts: Vec<ArtifactSummary>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let Ok(shown) = turned_down(deps, &job).await else {
        let said = "Stopped: the rule, or what was turned down, is gone.";
        return close(deps, task, &mut job, JobState::Cancelled, said, now).await;
    };
    let written = match draft_artifact(&steps, &artifacts) {
        Some(found) => parse_rule(&body_of(deps, task, found).await?, &shown.title),
        None => None,
    };
    if written.is_none() && matches!(stage(&steps), Stage::Unchecked) {
        let said = format!("Dropped: it wrote no rule that could be used. {WORDS_STAY}");
        return close(deps, task, &mut job, JobState::Dropped, &said, now).await;
    }
    let (state, verdict) = match critic_loop(deps, task, &mut job, &steps, &artifacts, now).await? {
        Loop::Added => return Ok(()),
        Loop::Settle(state, verdict) => (state, verdict),
    };
    let (state, said) = match (state, written) {
        (JobState::Passed, Some(text)) if rewrite(deps, &shown.rule, &text, now).await? => {
            let passed = job_outcome_line(&job, verdict.as_ref());
            (state, format!("{passed} The rule now reads: {text}."))
        }
        (JobState::Passed, _) => (
            JobState::Dropped,
            format!("Dropped: the rule could not be kept. {WORDS_STAY}"),
        ),
        (JobState::Failed, _) => (state, format!("{} {WORDS_STAY}", why_no_draft(&steps))),
        (state, _) => (
            state,
            format!("{} {WORDS_STAY}", job_outcome_line(&job, verdict.as_ref())),
        ),
    };
    close(deps, task, &mut job, state, &said, now).await
}
