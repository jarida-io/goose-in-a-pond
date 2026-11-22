//! The hourly review's side of intentions: what its brief is shown of a member's jobs, and
//! what becomes of the jobs its answer leaves: each is checked in code, the survivors go to
//! one critic call, and what is left is stored.

use std::collections::HashSet;

use chrono::{DateTime, Utc};

use pond_core::shared::domain::agent_note::{is_stale, AgentNote, NoteKind};
use pond_core::shared::domain::todo::{todo_key, StateFilter, Tz};
use pond_core::user_data::domain::anchor::{Anchor, AnchorState};
use pond_core::user_data::domain::engine_job::{EngineJob, JobClass, JobKind, JobState};
use pond_core::user_data::domain::intention::{text_key, Intention, IntentionState};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::services::intentions::{
    build_intent_critic_message, parse_intent_drops, parse_intents, room_for, validate,
    ConditionContext, PlaceChoice, INTENT_CRITIC_SYSTEM,
};
use pond_core::user_data::services::proactive_review::plans_lines;

use super::IntentionDeps;
use crate::engine_runner::ModelStop;

const NO_ACT_OR_WORDS: &str = "it names no act the heartbeat has, or has no words";
const UNREADABLE_CHECK: &str = "the check's reply could not be read, so none was kept";
const CUT_CHECK: &str = "someone began talking before the check finished, so none was kept";
const NO_CHECK: &str = "the check could not be run, so none was kept";
const UNREADABLE_LISTS: &str =
    "what is already waiting, on their to-dos or scheduled could not be read";
const NO_ROOM: &str = "as many jobs as may wait at once are already waiting";

/// What a review's brief is shown of one member's jobs.
#[derive(Debug, Clone, Default)]
pub struct ReviewContext {
    /// What a job's condition may name.
    pub conditions: ConditionContext,
    pub waiting: Vec<Intention>,
    /// The member's active anchors.
    pub plans: Vec<Anchor>,
    /// Notes the chat model left for the review; [`close_review`] deletes them.
    pub notes: Vec<AgentNote>,
}

async fn conditions(deps: &IntentionDeps, profile_id: &str) -> ConditionContext {
    let places = match deps.todos.places(profile_id).await {
        Ok(places) => places,
        Err(e) => {
            tracing::warn!(error = %e, "a member's places could not be read for a review");
            Vec::new()
        }
    };
    ConditionContext {
        places: places
            .into_iter()
            .map(|place| PlaceChoice {
                id: place.id,
                label: place.label,
            })
            .collect(),
        things: deps.things.things().await,
    }
}

/// The notes the member's conversations left for the review; one nobody came to in a month
/// is dropped here.
async fn notes_for(deps: &IntentionDeps, profile_id: &str, now: DateTime<Utc>) -> Vec<AgentNote> {
    let scope = ProfileScope::Owner(profile_id.to_string());
    let mut notes = deps
        .notes
        .list(&scope, NoteKind::Note)
        .await
        .unwrap_or_default();
    for stale in notes.iter().filter(|note| is_stale(note, now)) {
        let _ = deps.notes.remove(&stale.id, &scope).await;
    }
    notes.retain(|note| !is_stale(note, now));
    notes
}

/// Reads what the review of `profile_id` is shown; a store that cannot be read shows nothing
/// of its part.
pub async fn review_context(
    deps: &IntentionDeps,
    profile_id: &str,
    now: DateTime<Utc>,
) -> ReviewContext {
    ReviewContext {
        conditions: conditions(deps, profile_id).await,
        waiting: deps
            .intentions
            .list(profile_id, Some(IntentionState::Waiting))
            .await
            .unwrap_or_default(),
        plans: deps
            .engine
            .anchors
            .list(profile_id, &[AnchorState::Active])
            .await
            .unwrap_or_default(),
        notes: notes_for(deps, profile_id, now).await,
    }
}

/// What became of the jobs one review left itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kept {
    /// Jobs the answer held, readable or not.
    pub read: usize,
    pub stored: Vec<Intention>,
    /// Why each of the others was not kept, in plain words.
    pub not_kept: Vec<String>,
}

/// What the member's waiting jobs, open to-dos and schedules already say, as keys.
async fn taken_keys(
    deps: &IntentionDeps,
    profile_id: &str,
    waiting: &[Intention],
) -> anyhow::Result<HashSet<String>> {
    let mut taken: HashSet<String> = waiting.iter().map(Intention::key).collect();
    let open = deps.todos.list(profile_id, StateFilter::Open).await?;
    taken.extend(open.iter().map(|todo| todo_key(&todo.text)));
    let scheduled = deps.schedules.words_for(profile_id).await?;
    taken.extend(scheduled.iter().map(|words| text_key(words)));
    taken.remove("");
    Ok(taken)
}

/// The drafts that pass every check code can make, up to the room the member has left.
async fn checked(
    deps: &IntentionDeps,
    profile_id: &str,
    answer: &str,
    plans: &[Anchor],
    now: DateTime<Utc>,
    zone: Tz,
    kept: &mut Kept,
) -> Vec<Intention> {
    let parse = parse_intents(answer);
    kept.read = parse.drafts.len() + parse.unreadable;
    kept.not_kept
        .extend((0..parse.unreadable).map(|_| NO_ACT_OR_WORDS.to_string()));
    if parse.drafts.is_empty() {
        return Vec::new();
    }
    let waiting = deps
        .intentions
        .list(profile_id, Some(IntentionState::Waiting))
        .await;
    let read = match waiting {
        Ok(waiting) => taken_keys(deps, profile_id, &waiting)
            .await
            .map(|taken| (waiting.len(), taken)),
        Err(e) => Err(e),
    };
    let (waiting, mut taken) = match read {
        Ok(read) => read,
        Err(e) => {
            tracing::warn!(error = %e, "a review's jobs could not be checked against what exists");
            kept.not_kept.push(UNREADABLE_LISTS.to_string());
            return Vec::new();
        }
    };
    let ctx = conditions(deps, profile_id).await;
    let room = room_for(waiting);
    let mut survivors: Vec<Intention> = Vec::new();
    for draft in &parse.drafts {
        if survivors.len() == room {
            kept.not_kept.push(NO_ROOM.to_string());
            break;
        }
        match validate(draft, &ctx, &taken, plans, profile_id, now, zone) {
            Ok(job) => {
                taken.insert(job.key());
                survivors.push(job);
            }
            Err(refusal) => kept.not_kept.push(refusal.as_str().to_string()),
        }
    }
    survivors
}

/// One critic call on `survivors`; whether it answered, and the jobs it left.
async fn criticised(
    deps: &IntentionDeps,
    survivors: Vec<Intention>,
    happened: &str,
    plans: &str,
    kept: &mut Kept,
) -> (bool, Vec<Intention>) {
    let message = build_intent_critic_message(&survivors, happened, plans);
    let reply = match deps
        .engine
        .model
        .complete(INTENT_CRITIC_SYSTEM, message)
        .await
    {
        Ok(reply) => reply,
        Err(stop) => {
            let why = match stop {
                ModelStop::Cut => CUT_CHECK,
                ModelStop::Failed(_) => NO_CHECK,
            };
            kept.not_kept.push(why.to_string());
            return (false, Vec::new());
        }
    };
    let Some(drops) = parse_intent_drops(&reply, survivors.len()) else {
        kept.not_kept.push(UNREADABLE_CHECK.to_string());
        return (true, Vec::new());
    };
    let mut left = Vec::new();
    for (place, job) in survivors.into_iter().enumerate() {
        match drops.iter().find(|(dropped, _)| *dropped == place) {
            Some((_, why)) if why.is_empty() => {
                kept.not_kept.push("the check dropped it".to_string())
            }
            Some((_, why)) => kept.not_kept.push(format!("the check dropped it: {why}")),
            None => left.push(job),
        }
    }
    (true, left)
}

fn outcome_line(kept: &Kept) -> String {
    let jobs = match kept.read {
        1 => "1 job".to_string(),
        read => format!("{read} jobs"),
    };
    let mut line = match kept.stored.len() {
        0 => format!("Kept none of {jobs}."),
        stored => format!("Kept {stored} of {jobs}."),
    };
    if !kept.not_kept.is_empty() {
        line.push_str(&format!(" Not kept: {}.", kept.not_kept.join("; ")));
    }
    line
}

/// Checks the jobs a review's answer left, stores the ones that pass and logs the run as one
/// engine job.
async fn keep_jobs(
    deps: &IntentionDeps,
    profile_id: &str,
    answer: &str,
    happened: &str,
    started: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Kept {
    let mut kept = Kept::default();
    let zone = deps.zone().await;
    let plans = deps
        .engine
        .anchors
        .list(profile_id, &[AnchorState::Active])
        .await
        .unwrap_or_default();
    let survivors = checked(deps, profile_id, answer, &plans, now, zone, &mut kept).await;
    if kept.read == 0 {
        return kept;
    }
    let (asked, left) = match survivors.is_empty() {
        true => (false, survivors),
        false => {
            let plans = plans_lines(&plans, zone);
            criticised(deps, survivors, happened, &plans, &mut kept).await
        }
    };
    for job in left {
        match deps.intentions.add(&job).await {
            Ok(true) => kept.stored.push(job),
            Ok(false) => kept
                .not_kept
                .push("a waiting job already says it".to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "a job a review left could not be stored");
                kept.not_kept.push("it could not be stored".to_string());
            }
        }
    }

    // Added already settled: a queued job with no task is one the engine's tick opens.
    let mut job = EngineJob::new(
        JobKind::Heartbeat,
        JobClass::Upkeep,
        Some(profile_id),
        "",
        started,
    );
    let _ = job.start(None, started);
    if asked {
        job.count_round(Vec::new());
    }
    let state = match kept.stored.is_empty() {
        true => JobState::Dropped,
        false => JobState::Passed,
    };
    let _ = job.settle(state, &outcome_line(&kept), now);
    if let Err(e) = deps.engine.jobs.add(&job).await {
        tracing::warn!(error = %e, "a review's jobs could not be logged");
    }
    tracing::info!(
        target: "giap::trace",
        kind = "heartbeat_jobs",
        member = %profile_id,
        read = kept.read,
        stored = kept.stored.len(),
        not_kept = kept.not_kept.len(),
        critic_answered = asked,
        "a review's jobs were checked"
    );
    kept
}

/// What follows a review that answered: the jobs it left are checked and kept, and the notes
/// it was shown are deleted whatever it made of them.
pub async fn close_review(
    deps: &IntentionDeps,
    profile_id: &str,
    answer: &str,
    shown: &ReviewContext,
    happened: &str,
    started: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Kept {
    let kept = keep_jobs(deps, profile_id, answer, happened, started, now).await;
    let scope = ProfileScope::Owner(profile_id.to_string());
    for note in &shown.notes {
        if let Err(e) = deps.notes.remove(&note.id, &scope).await {
            tracing::warn!(error = %e, "a note a review was shown could not be deleted");
        }
    }
    kept
}
