//! A capture job: the maker that reads one picture a member's phone sent, the critic shown the
//! same picture, and what becomes of it: kept with what it shows, held for the Budget agent, or
//! deleted whole.

use chrono::{DateTime, Utc};

use pond_core::mcp::ports::notification::Notification;
use pond_core::shared::domain::long_task::{ArtifactKind, ArtifactSummary, LongTask, PlanStep};
use pond_core::shared::domain::todo::zone;
use pond_core::user_data::domain::anchor::AnchorOrigin;
use pond_core::user_data::domain::capture::{Capture, Sensitivity};
use pond_core::user_data::domain::engine_job::{EngineJob, JobState};
use pond_core::user_data::domain::specialist::{
    AgentSwitch, SpecialistId, MONEY_CAPTURES_BEFORE_OFFER,
};
use pond_core::user_data::services::capture_reading::{
    build_read_critic_message, parse_read_verdict, parse_reading, settle_reading, ReadOutcome,
    ReadVerdict, Reading, READ_CRITIC_SYSTEM, READ_MESSAGE, READ_SYSTEM,
};
use pond_core::user_data::services::engine::critic::Verdict;
use pond_core::user_data::services::engine::settle::job_outcome_line;
use pond_core::user_data::services::engine::steps::{
    critic_step, draft_artifact, verdict_artifact,
};
use pond_core::user_data::services::extraction_gate::{anchor_from_plan, PlanCandidate};
use pond_core::user_data::services::ledger_reading::should_offer_budget;

use super::finish::{stage, Stage};
use super::inline::{body_of, failed, Saved, StepCtx};
use super::pictures::{
    can_read, capture_of, close_and_forget_task, forget_capture, picture_kept, picture_of,
    queue_ledger, ring_indexer,
};
use super::{notices, EngineDeps, ModelStop};

const READING_TITLE: &str = "Reading";
const CHECK_TITLE: &str = "Check";
/// Money pictures held while the Budget agent is not on: as many as it takes to offer it.
const MAX_HELD: usize = MONEY_CAPTURES_BEFORE_OFFER as usize;
const PRIVATE: &str = "Dropped: the picture was private, so nothing of it was kept.";
const GONE: &str = "Stopped: the picture was deleted.";
const WAITS: &str = "Failed: this pond cannot read pictures now; the picture waits until it can.";

/// The maker: what the picture shows and what the member seems to want with it.
pub(super) async fn make(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let (_, picture) = picture_of(ctx.deps, ctx.job).await?;
    let reply = ctx
        .ask_seeing(READ_SYSTEM, READ_MESSAGE.to_string(), picture)
        .await?;
    let summary = match parse_reading(&reply) {
        None => "The reading could not be read.",
        Some(Reading::Private) => "Found the picture private.",
        Some(Reading::Kept {
            sensitive: Sensitivity::Money,
            ..
        }) => "Found money in the picture.",
        Some(Reading::Kept { .. }) => "Read what the picture shows.",
    };
    let saved = ctx
        .save(ArtifactKind::Data, READING_TITLE, summary, reply)
        .await?;
    Ok(Saved {
        summary: summary.to_string(),
        artifacts: vec![saved],
    })
}

/// The critic: the reading checked against the same picture.
pub(super) async fn critic(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let (_, draft) = ctx.draft().await?;
    let reading = parse_reading(&draft).ok_or_else(|| failed("the reading could not be read"))?;
    let (_, picture) = picture_of(ctx.deps, ctx.job).await?;
    let reply = ctx
        .ask_seeing(
            READ_CRITIC_SYSTEM,
            build_read_critic_message(&reading),
            picture,
        )
        .await?;
    let summary = match parse_read_verdict(&reply) {
        ReadVerdict::Agree => "The check agreed with the reading.",
        ReadVerdict::Disagree(_) => "The check disagreed with the reading.",
        ReadVerdict::Private => "The check found the picture private.",
        ReadVerdict::Unreadable => "The check could not be read.",
    };
    let saved = ctx
        .save(ArtifactKind::Data, CHECK_TITLE, summary, reply)
        .await?;
    Ok(Saved {
        summary: summary.to_string(),
        artifacts: vec![saved],
    })
}

/// What comes once the job is closed.
enum Then {
    Nothing,
    ReadPayments,
    OfferBudget(usize),
}

struct Settled {
    state: JobState,
    outcome: String,
    then: Then,
}

impl Settled {
    fn new(state: JobState, outcome: impl Into<String>) -> Self {
        Self {
            state,
            outcome: outcome.into(),
            then: Then::Nothing,
        }
    }
}

async fn forget(deps: &EngineDeps, capture: &Capture, outcome: &str) -> anyhow::Result<Settled> {
    forget_capture(deps, capture).await?;
    Ok(Settled::new(JobState::Dropped, outcome))
}

/// Counts a failed read; the second deletes the picture and its row.
async fn retry(
    deps: &EngineDeps,
    mut capture: Capture,
    state: JobState,
    why: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Settled> {
    let lead = match state {
        JobState::Failed => "Failed",
        _ => "Dropped",
    };
    capture.count_attempt(now);
    if capture.is_unreadable() {
        forget_capture(deps, &capture).await?;
        let said = format!("{lead}: {why}. It was tried twice, so the picture was deleted.");
        return Ok(Settled::new(state, said));
    }
    deps.captures.save(&capture).await?;
    Ok(Settled::new(
        state,
        format!("{lead}: {why}. It will be read once more."),
    ))
}

/// A plan the picture shows becomes a candidate anchor of its member's; `true` when one was
/// added.
async fn offer_plan(
    deps: &EngineDeps,
    capture: &Capture,
    plan: &PlanCandidate,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let (timezone, _) = deps.clock_and_place().await;
    let said_at = capture.taken_at.unwrap_or(capture.created_at);
    let member = capture.profile_id.as_str();
    let Some(anchor) = anchor_from_plan(
        plan,
        member,
        AnchorOrigin::Capture,
        Some(&capture.id),
        said_at,
        zone(&timezone),
        now,
    ) else {
        return Ok(false);
    };
    // In any state: one the member stopped, or let fade, never comes back from a picture.
    if deps
        .anchors
        .by_title(member, &anchor.key())
        .await?
        .is_some()
    {
        return Ok(false);
    }
    deps.anchors.add(&anchor).await
}

/// A money picture: held for the Budget agent, undescribed, or deleted when it cannot be.
async fn hold_for_budget(
    deps: &EngineDeps,
    mut capture: Capture,
    passed: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Settled> {
    let member = capture.profile_id.clone();
    let switch = deps.agents.switch(&member, SpecialistId::Budget).await?;
    if switch == AgentSwitch::Declined {
        let said =
            "Dropped: it shows money and the Budget agent was turned down, so it was deleted.";
        return forget(deps, &capture, said).await;
    }
    if !switch.is_on() && deps.captures.hot(&member).await?.len() >= MAX_HELD {
        let said = "Dropped: it shows money, and enough such pictures already wait for the \
                    Budget agent, so it was deleted.";
        return forget(deps, &capture, said).await;
    }
    capture.hold_for_budget(now);
    deps.captures.save(&capture).await?;
    if switch.is_on() {
        return Ok(Settled {
            state: JobState::Passed,
            outcome: format!("{passed} It shows money; the Budget agent reads it next."),
            then: Then::ReadPayments,
        });
    }
    let held = deps.captures.hot(&member).await?.len();
    Ok(Settled {
        state: JobState::Passed,
        outcome: format!("{passed} It shows money; it waits for the Budget agent."),
        then: if should_offer_budget(held, switch) {
            Then::OfferBudget(held)
        } else {
            Then::Nothing
        },
    })
}

async fn keep(
    deps: &EngineDeps,
    job: &EngineJob,
    mut capture: Capture,
    reading: Reading,
    now: DateTime<Utc>,
) -> anyhow::Result<Settled> {
    let passed = job_outcome_line(
        job,
        Some(&Verdict {
            pass: true,
            objections: Vec::new(),
        }),
    );
    let Reading::Kept {
        sensitive,
        description,
        intent,
        details,
        plan,
    } = reading
    else {
        return forget(deps, &capture, PRIVATE).await;
    };
    if sensitive == Sensitivity::Money {
        return hold_for_budget(deps, capture, &passed, now).await;
    }
    capture.read(&description, Some(&intent), details, now);
    deps.captures.save(&capture).await?;
    ring_indexer(deps);
    let offered = match &plan {
        Some(plan) => offer_plan(deps, &capture, plan, now).await?,
        None => false,
    };
    let outcome = if offered {
        format!("{passed} Kept what it shows, and offered the plan in it.")
    } else {
        format!("{passed} Kept what it shows.")
    };
    Ok(Settled::new(JobState::Passed, outcome))
}

async fn as_settled(
    deps: &EngineDeps,
    job: &EngineJob,
    capture: Capture,
    outcome: ReadOutcome,
    why: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Settled> {
    match outcome {
        ReadOutcome::Forget => forget(deps, &capture, PRIVATE).await,
        ReadOutcome::Retry => retry(deps, capture, JobState::Dropped, why, now).await,
        ReadOutcome::Keep(reading) => keep(deps, job, capture, reading, now).await,
    }
}

fn in_words(count: usize) -> String {
    match count {
        1 => "one".to_string(),
        2 => "two".to_string(),
        3 => "three".to_string(),
        4 => "four".to_string(),
        count => count.to_string(),
    }
}

/// The one notice that offers a member the Budget agent; the phone answers it with
/// `PUT /me/agents/budget`.
fn budget_offer(member: &str, held: usize, now: DateTime<Utc>) -> Notification {
    let mut fields = serde_json::Map::new();
    fields.insert("action".into(), "agent_offer".into());
    fields.insert("agent".into(), SpecialistId::Budget.as_str().into());
    fields.insert("label".into(), "Turn on".into());
    Notification {
        id: format!("agent-offer-budget-{member}"),
        target: String::new(),
        category: "action_required".to_string(),
        title: "Keep a record of what you spend?".to_string(),
        body: format!(
            "You have sent {} pictures of receipts or payments. Turn on the Budget agent and \
             Goose will keep a ledger from them. If you say no, they are deleted.",
            in_words(held)
        ),
        timestamp: now.to_rfc3339(),
        data: Some(serde_json::Value::Object(fields)),
    }
}

async fn follow_up(
    deps: &EngineDeps,
    capture_id: &str,
    member: &str,
    then: Then,
    now: DateTime<Utc>,
) {
    let done = match then {
        Then::Nothing => Ok(()),
        Then::ReadPayments => match deps.captures.get(member, capture_id).await {
            Ok(Some(capture)) => queue_ledger(deps, &capture, now).await.map(|_| ()),
            Ok(None) => Ok(()),
            Err(e) => Err(e),
        },
        Then::OfferBudget(held) => {
            let offered = deps
                .agents
                .set(member, SpecialistId::Budget, AgentSwitch::Offered, now)
                .await;
            if offered.is_ok() {
                notices::tell_member(deps, member, budget_offer(member, held, now), None).await;
            }
            offered
        }
    };
    if let Err(e) = done {
        tracing::warn!(member = %member, error = %e, "what follows a read picture could not be done");
    }
}

/// Why the maker gave no reading, in the words its failed step left.
fn failure_of(steps: &[PlanStep]) -> &str {
    steps
        .iter()
        .rev()
        .find_map(|step| step.failure())
        .unwrap_or("its step did not run")
}

fn reading_outcome_kind(settled: &Settled) -> &'static str {
    match (&settled.state, &settled.then) {
        (JobState::Passed, Then::Nothing) => "kept",
        (JobState::Passed, _) => "money",
        (JobState::Failed, _) => "failed",
        (JobState::Cancelled, _) => "gone",
        _ => "dropped",
    }
}

/// What follows a capture task's steps: its one check, then the picture is kept, held for
/// Budget, read again or deleted.
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
    let reading = match draft_artifact(&steps, &artifacts).filter(|_| !matches!(at, Stage::NoDraft))
    {
        Some(found) => parse_reading(&body_of(deps, task, found).await?),
        None => None,
    };
    let (member, capture_id) = (capture.profile_id.clone(), capture.id.clone());
    let settled = match at {
        Stage::NoDraft if !can_read(deps).await => Settled::new(JobState::Failed, WAITS),
        Stage::NoDraft if !picture_kept(deps, &capture).await => {
            forget_capture(deps, &capture).await?;
            Settled::new(JobState::Dropped, "Dropped: its picture is gone.")
        }
        Stage::NoDraft => {
            let why = failure_of(&steps);
            retry(deps, capture, JobState::Failed, why, now).await?
        }
        Stage::Unchecked => match &reading {
            Some(Reading::Kept { .. }) => match critic_step(task, &steps, &artifacts) {
                Ok(plan) => return deps.tasks.replace_open_steps(&plan).await,
                Err(_) => {
                    let why = "its check could not be added";
                    retry(deps, capture, JobState::Dropped, why, now).await?
                }
            },
            Some(Reading::Private) => forget(deps, &capture, PRIVATE).await?,
            None => {
                let why = "the reading could not be read";
                retry(deps, capture, JobState::Dropped, why, now).await?
            }
        },
        Stage::CheckDied => {
            let outcome = settle_reading(reading, ReadVerdict::Unreadable);
            let why = "its check did not finish";
            as_settled(deps, &job, capture, outcome, why, now).await?
        }
        Stage::Checked => {
            let reply = match verdict_artifact(&steps, &artifacts) {
                Some(found) => body_of(deps, task, found).await?,
                None => String::new(),
            };
            job.count_round(Vec::new());
            let verdict = parse_read_verdict(&reply);
            // The check's own words about the picture are never kept in the log.
            let why = match verdict {
                ReadVerdict::Unreadable => "the check could not be read",
                _ => "the check disagreed with the reading",
            };
            let outcome = settle_reading(reading, verdict);
            as_settled(deps, &job, capture, outcome, why, now).await?
        }
    };
    tracing::info!(
        target: "giap::trace",
        kind = "engine_capture_read",
        job = %job.id,
        outcome = reading_outcome_kind(&settled),
        "a picture's reading settled"
    );
    close_and_forget_task(deps, task, &mut job, settled.state, &settled.outcome, now).await?;
    follow_up(deps, &capture_id, &member, settled.then, now).await;
    Ok(())
}

/// A capture task the runner gave up on: the attempt counts, unless the pond cannot read
/// pictures at all now.
pub(super) async fn out_of_road(
    deps: &EngineDeps,
    task: &LongTask,
    mut job: EngineJob,
    reason: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let settled = match capture_of(deps, &job).await? {
        None => Settled::new(JobState::Cancelled, GONE),
        Some(_) if !can_read(deps).await => Settled::new(JobState::Failed, WAITS),
        Some(capture) => retry(deps, capture, JobState::Failed, reason, now).await?,
    };
    close_and_forget_task(deps, task, &mut job, settled.state, &settled.outcome, now).await
}
